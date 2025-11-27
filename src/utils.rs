use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use chrono::{DateTime, Datelike, Local, Utc};
use num_format::{Locale, ToFormattedString};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};

use crate::types::{ConversationMessage, DailyStats};

static WARNED_MESSAGES: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

pub fn warn_once(message: impl Into<String>) {
    let message = message.into();
    let cache = WARNED_MESSAGES.get_or_init(|| Mutex::new(HashSet::new()));

    if let Ok(mut warned) = cache.lock()
        && warned.insert(message.clone())
    {
        eprintln!("{message}");
    }
}

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

pub fn format_date_for_display(date: &str) -> String {
    if date == "unknown" {
        return "Unknown".to_string();
    }

    if let Ok(parsed) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") {
        // Format with non-padded month and day
        let month = parsed.month();
        let day = parsed.day();
        let year = parsed.year();
        let formatted = format!("{month}/{day}/{year}");

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
    let mut conversation_start_dates: BTreeMap<String, String> = BTreeMap::new();

    for entry in entries {
        let timestamp = &entry.date.with_timezone(&Local);
        let conversation_hash = &entry.conversation_hash;
        let date = timestamp.format("%Y-%m-%d").to_string();

        // Only update if this is earlier than what we've seen, or if we haven't seen this
        // conversation before.  This is to handle the case where a conversation spans
        // multiple days, we'd want to ascribe it to the day on which it was started.
        conversation_start_dates
            .entry(conversation_hash.clone())
            .and_modify(|existing_date| {
                if date < *existing_date {
                    *existing_date = date.clone();
                }
            })
            .or_insert(date.clone());

        let daily_stats_entry = daily_stats
            .entry(date.clone())
            .or_insert_with(|| DailyStats {
                date: date.clone(),
                ..Default::default()
            });

        match &entry.model {
            Some(model) => {
                // AI message
                daily_stats_entry.ai_messages += 1;
                *daily_stats_entry
                    .models
                    .entry(model.to_string())
                    .or_insert(0) += 1;

                // Aggregate all stats
                daily_stats_entry.stats.cost += entry.stats.cost;
                daily_stats_entry.stats.input_tokens += entry.stats.input_tokens;
                daily_stats_entry.stats.output_tokens += entry.stats.output_tokens;
                daily_stats_entry.stats.reasoning_tokens += entry.stats.reasoning_tokens;
                daily_stats_entry.stats.cache_creation_tokens += entry.stats.cache_creation_tokens;
                daily_stats_entry.stats.cache_read_tokens += entry.stats.cache_read_tokens;
                daily_stats_entry.stats.cached_tokens += entry.stats.cached_tokens;
                daily_stats_entry.stats.tool_calls += entry.stats.tool_calls;
                daily_stats_entry.stats.terminal_commands += entry.stats.terminal_commands;
                daily_stats_entry.stats.file_searches += entry.stats.file_searches;
                daily_stats_entry.stats.file_content_searches += entry.stats.file_content_searches;
                daily_stats_entry.stats.files_read += entry.stats.files_read;
                daily_stats_entry.stats.files_added += entry.stats.files_added;
                daily_stats_entry.stats.files_edited += entry.stats.files_edited;
                daily_stats_entry.stats.files_deleted += entry.stats.files_deleted;
                daily_stats_entry.stats.lines_read += entry.stats.lines_read;
                daily_stats_entry.stats.lines_added += entry.stats.lines_added;
                daily_stats_entry.stats.lines_edited += entry.stats.lines_edited;
                daily_stats_entry.stats.lines_deleted += entry.stats.lines_deleted;
                daily_stats_entry.stats.bytes_read += entry.stats.bytes_read;
                daily_stats_entry.stats.bytes_added += entry.stats.bytes_added;
                daily_stats_entry.stats.bytes_edited += entry.stats.bytes_edited;
                daily_stats_entry.stats.bytes_deleted += entry.stats.bytes_deleted;
                daily_stats_entry.stats.todos_created += entry.stats.todos_created;
                daily_stats_entry.stats.todos_completed += entry.stats.todos_completed;
                daily_stats_entry.stats.todos_in_progress += entry.stats.todos_in_progress;
                daily_stats_entry.stats.todo_writes += entry.stats.todo_writes;
                daily_stats_entry.stats.todo_reads += entry.stats.todo_reads;
                daily_stats_entry.stats.code_lines += entry.stats.code_lines;
                daily_stats_entry.stats.docs_lines += entry.stats.docs_lines;
                daily_stats_entry.stats.data_lines += entry.stats.data_lines;
                daily_stats_entry.stats.media_lines += entry.stats.media_lines;
                daily_stats_entry.stats.config_lines += entry.stats.config_lines;
                daily_stats_entry.stats.other_lines += entry.stats.other_lines;
            }
            None => {
                // User message
                daily_stats_entry.user_messages += 1;

                // Aggregate user stats too (mostly todo-related)
                daily_stats_entry.stats.todos_created += entry.stats.todos_created;
                daily_stats_entry.stats.todos_completed += entry.stats.todos_completed;
                daily_stats_entry.stats.todos_in_progress += entry.stats.todos_in_progress;
                daily_stats_entry.stats.todo_writes += entry.stats.todo_writes;
                daily_stats_entry.stats.todo_reads += entry.stats.todo_reads;
            }
        };
    }

    // Track conversations started on each date and update daily stats
    for start_date in conversation_start_dates.values() {
        if let Some(daily_stats_entry) = daily_stats.get_mut(start_date) {
            daily_stats_entry.conversations += 1;
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

/// Filters messages to only include those created after a specific date
pub async fn get_messages_later_than(
    date: i64,
    messages: Vec<ConversationMessage>,
) -> Result<Vec<ConversationMessage>> {
    let mut messages_later_than_date = Vec::new();
    for msg in messages {
        if msg.date.timestamp_millis() >= date {
            messages_later_than_date.push(msg);
        }
    }

    Ok(messages_later_than_date)
}

/// Filters messages to only include those with zero (or near-zero) cost
pub fn filter_zero_cost_messages(messages: Vec<ConversationMessage>) -> Vec<ConversationMessage> {
    const EPSILON: f64 = 1e-10;
    messages
        .into_iter()
        .filter(|msg| msg.stats.cost.abs() < EPSILON)
        .collect()
}

pub fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text);
    format!("{:x}", hasher.finalize())
}

/// Custom serde deserializer for RFC3339 timestamp strings to DateTime<Utc>
pub fn deserialize_utc_timestamp<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.into())
        .map_err(serde::de::Error::custom)
}

#[cfg(test)]
mod tests;
