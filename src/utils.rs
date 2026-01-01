use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use chrono::{DateTime, Datelike, Local, Utc};
use num_format::{Locale, ToFormattedString};
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use xxhash_rust::xxh3::xxh3_64;

use crate::types::{CompactDate, ConversationMessage, DailyStats};

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

/// Format a number for display. Accepts both u32 and u64.
pub fn format_number(n: impl Into<u64>, options: &NumberFormatOptions) -> String {
    let n: u64 = n.into();
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
                date: CompactDate::from_local(&entry.date),
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

                // Aggregate TUI-relevant stats only (TuiStats has 6 fields)
                daily_stats_entry.stats.add_cost(entry.stats.cost);
                daily_stats_entry.stats.input_tokens = daily_stats_entry
                    .stats
                    .input_tokens
                    .saturating_add(entry.stats.input_tokens as u32);
                daily_stats_entry.stats.output_tokens = daily_stats_entry
                    .stats
                    .output_tokens
                    .saturating_add(entry.stats.output_tokens as u32);
                daily_stats_entry.stats.reasoning_tokens = daily_stats_entry
                    .stats
                    .reasoning_tokens
                    .saturating_add(entry.stats.reasoning_tokens as u32);
                daily_stats_entry.stats.cached_tokens = daily_stats_entry
                    .stats
                    .cached_tokens
                    .saturating_add(entry.stats.cached_tokens as u32);
                daily_stats_entry.stats.tool_calls = daily_stats_entry
                    .stats
                    .tool_calls
                    .saturating_add(entry.stats.tool_calls);
            }
            None => {
                // User message - no TUI-relevant stats to aggregate
                daily_stats_entry.user_messages += 1;
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
                        date: CompactDate::from_str(&date_str).unwrap_or_default(),
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

/// Fast hash for local deduplication only (NOT for cloud - use hash_text for global_hash)
pub fn fast_hash(text: &str) -> String {
    format!("{:016x}", xxh3_64(text.as_bytes()))
}

/// Parallel deduplication by global_hash using DashMap.
/// Used by copilot, cline, roo_code, kilo_code analyzers.
pub fn deduplicate_by_global_hash_parallel(
    messages: Vec<ConversationMessage>,
) -> Vec<ConversationMessage> {
    use dashmap::DashMap;
    use rayon::iter::{IntoParallelIterator, ParallelIterator};

    let seen: DashMap<String, ()> = DashMap::with_capacity(messages.len() / 2);
    messages
        .into_par_iter()
        .filter(|msg| seen.insert(msg.global_hash.clone(), ()).is_none())
        .collect()
}

/// Parallel deduplication by local_hash using DashMap.
/// Used by gemini_cli, qwen_code analyzers.
/// Messages without local_hash are always kept.
pub fn deduplicate_by_local_hash_parallel(
    messages: Vec<ConversationMessage>,
) -> Vec<ConversationMessage> {
    use dashmap::DashMap;
    use rayon::iter::{IntoParallelIterator, ParallelIterator};

    let seen: DashMap<String, ()> = DashMap::with_capacity(messages.len() / 2);
    messages
        .into_par_iter()
        .filter(|msg| {
            if let Some(local_hash) = &msg.local_hash {
                seen.insert(local_hash.clone(), ()).is_none()
            } else {
                true // Always keep messages without local_hash
            }
        })
        .collect()
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

/// Get the system's local timezone as an IANA timezone string (e.g., "America/Chicago")
pub fn get_local_timezone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

#[cfg(test)]
mod tests;
