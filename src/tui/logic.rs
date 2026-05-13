/// Logic module for TUI data processing and aggregation.
///
/// Provides functions to aggregate statistics, filter dates, and check for data presence.
use crate::types::{
    CompactDate, ConversationMessage, DailyStats, MessageRole, ModelCounts, Stats, TuiStats,
    intern_model,
};
use chrono::{Datelike, NaiveDate, Weekday};
use std::collections::BTreeMap;
use std::sync::Arc;

// Re-export SessionAggregate from types
pub use crate::types::SessionAggregate;

/// Accumulate TUI-relevant stats from a full Stats into a TuiStats.
/// Only copies the 6 fields displayed in the TUI.
pub fn accumulate_tui_stats(dst: &mut TuiStats, src: &Stats) {
    dst.input_tokens = dst.input_tokens.saturating_add(src.input_tokens);
    dst.output_tokens = dst.output_tokens.saturating_add(src.output_tokens);
    dst.reasoning_tokens = dst.reasoning_tokens.saturating_add(src.reasoning_tokens);
    dst.cached_tokens = dst.cached_tokens.saturating_add(src.cached_tokens);
    dst.add_cost(src.cost);
    dst.tool_calls = dst.tool_calls.saturating_add(src.tool_calls);
}

fn parse_period_parts(day: &str) -> Option<(u32, u32, Option<u32>)> {
    let parts: Vec<&str> = day.split('-').collect();

    match parts.as_slice() {
        [year, month] => Some((year.parse().ok()?, month.parse().ok()?, None)),
        [year, month, day_num] => Some((
            year.parse().ok()?,
            month.parse().ok()?,
            Some(day_num.parse().ok()?),
        )),
        _ => None,
    }
}

fn parse_input_date(buffer: &str) -> Option<NaiveDate> {
    let normalized = buffer.trim().replace('/', "-");
    let trimmed = normalized.trim_end_matches('-');
    let parts: Vec<&str> = trimmed.split('-').filter(|s| !s.is_empty()).collect();

    let [p0, p1, p2] = parts.as_slice() else {
        return None;
    };

    let (a, b, c) = (
        p0.parse::<u32>().ok()?,
        p1.parse::<u32>().ok()?,
        p2.parse::<u32>().ok()?,
    );

    let (year, month, day) = if a > 31 {
        (a as i32, b, c)
    } else if c > 31 {
        (c as i32, a, b)
    } else {
        return None;
    };

    NaiveDate::from_ymd_opt(year, month, day)
}

fn parse_week_key(period: &str) -> Option<(i32, u32)> {
    let (year, week) = period.split_once("-W")?;
    Some((year.parse().ok()?, week.parse().ok()?))
}

fn week_matches_buffer(week_year: i32, iso_week: u32, buffer: &str) -> bool {
    if let Some(date) = parse_input_date(buffer) {
        let input_week = date.iso_week();
        return input_week.year() == week_year && input_week.week() == iso_week;
    }

    let normalized = buffer
        .trim()
        .to_uppercase()
        .replace('/', "-")
        .replace(' ', "");
    let canonical = format!("{week_year:04}-W{iso_week:02}");

    if normalized == canonical
        || normalized == format!("{week_year:04}-W{iso_week}")
        || normalized == format!("W{iso_week:02}")
        || normalized == format!("W{iso_week}")
        || normalized == format!("WEEK{iso_week:02}")
        || normalized == format!("WEEK{iso_week}")
    {
        return true;
    }

    let parts: Vec<&str> = normalized.split('-').filter(|s| !s.is_empty()).collect();

    match parts.as_slice() {
        [single] => single
            .parse::<u32>()
            .map(|value| value == iso_week || value == week_year as u32)
            .unwrap_or(false),
        [year, week] => {
            if let (Ok(year), Ok(week)) = (year.parse::<i32>(), week.parse::<u32>()) {
                year == week_year && week == iso_week
            } else {
                false
            }
        }
        _ => false,
    }
}

fn year_matches_buffer(year: u32, buffer: &str) -> bool {
    if let Some(date) = parse_input_date(buffer) {
        return date.year() as u32 == year;
    }

    let normalized = buffer.trim().replace('/', "-");
    if normalized == year.to_string() {
        return true;
    }

    normalized
        .split('-')
        .filter(|s| !s.is_empty())
        .filter_map(|part| part.parse::<u32>().ok())
        .any(|value| value == year)
}

fn month_name_to_number(lower: &str) -> Option<u32> {
    match lower {
        s if "january".starts_with(s) && s.len() >= 3 => Some(1),
        s if "february".starts_with(s) && s.len() >= 3 => Some(2),
        s if "march".starts_with(s) && s.len() >= 3 => Some(3),
        s if "april".starts_with(s) && s.len() >= 3 => Some(4),
        s if "may".starts_with(s) && s.len() >= 3 => Some(5),
        s if "june".starts_with(s) && s.len() >= 3 => Some(6),
        s if "july".starts_with(s) && s.len() >= 3 => Some(7),
        s if "august".starts_with(s) && s.len() >= 3 => Some(8),
        s if "september".starts_with(s) && s.len() >= 3 => Some(9),
        s if "october".starts_with(s) && s.len() >= 3 => Some(10),
        s if "november".starts_with(s) && s.len() >= 3 => Some(11),
        s if "december".starts_with(s) && s.len() >= 3 => Some(12),
        _ => None,
    }
}

/// Check if a date string (YYYY-MM-DD format) matches the user's search buffer
pub fn date_matches_buffer(day: &str, buffer: &str) -> bool {
    if buffer.is_empty() {
        return true;
    }

    if let Some((week_year, iso_week)) = parse_week_key(day) {
        return week_matches_buffer(week_year, iso_week, buffer);
    }

    if let Ok(year) = day.parse::<u32>() {
        return year_matches_buffer(year, buffer);
    }

    let Some((day_year, day_month, day_number)) = parse_period_parts(day) else {
        return day == buffer;
    };

    if let Some(date) = parse_input_date(buffer) {
        return day_year == date.year() as u32
            && day_month == date.month()
            && day_number
                .map(|actual_day| actual_day == date.day())
                .unwrap_or(true);
    }

    // Check for month name match first
    let lower = buffer.to_lowercase();
    if let Some(month) = month_name_to_number(&lower) {
        return day_month == month;
    }

    let normalized_input = buffer.replace('/', "-");

    // Remove trailing separator for partial matches like "7/" or "7-"
    let trimmed = normalized_input.trim_end_matches('-');

    // Exact match
    if day == buffer || day == trimmed {
        return true;
    }

    let parts: Vec<&str> = trimmed.split('-').filter(|s| !s.is_empty()).collect();
    if parts.len() == 1 {
        // Single number - prefer month matching, but allow year-only lookups too.
        if let Ok(number) = parts[0].parse::<u32>() {
            if number > 31 {
                return day_year == number;
            }

            if (13..=31).contains(&number) {
                return day_number
                    .map(|day_value| day_value == number)
                    .unwrap_or(false);
            }

            if (1..=12).contains(&number) {
                return day_month == number;
            }
        }

        // Otherwise match if the date contains this string
        return day.contains(trimmed);
    } else if parts.len() == 2 {
        // Month and day only (M-D or MM-DD), Year-Month (YYYY-MM), or Month-Year (M-YYYY)
        if let (Ok(p1), Ok(p2)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            if p1 > 31 {
                // Assume Year-Month
                return day_year == p1 && day_month == p2;
            } else if p2 > 31 {
                // Assume Month-Year
                return day_month == p1 && day_year == p2;
            } else {
                // Assume Month-Day
                return day_number
                    .map(|day_value| day_month == p1 && day_value == p2)
                    .unwrap_or(false);
            }
        }
    } else if parts.len() == 3 {
        // Could be YYYY-M-D or M/D/YYYY
        if let (Ok(p0), Ok(p1), Ok(p2)) = (
            parts[0].parse::<u32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            // Determine format based on which part looks like a year
            let (year, month, expected_day) = if p0 > 31 {
                // YYYY-M-D format
                (p0, p1, p2)
            } else if p2 > 31 {
                // M/D/YYYY format
                (p2, p0, p1)
            } else {
                // Ambiguous, assume YYYY-M-D
                (p0, p1, p2)
            };

            return day_number
                .map(|actual_day| {
                    day_year == year && day_month == month && actual_day == expected_day
                })
                .unwrap_or(false);
        }
    }

    false
}

/// Roll up daily statistics into periods derived by `period_key_fn`.
///
/// `period_key_fn` returns a `(period_key, representative_date)` pair for each
/// daily row. All rows with the same key are then merged via
/// `DailyStats += &DailyStats`.
fn aggregate_daily_stats_by_period<F>(
    daily_stats: &BTreeMap<String, DailyStats>,
    mut period_key_fn: F,
) -> BTreeMap<String, DailyStats>
where
    F: FnMut(&DailyStats) -> (String, CompactDate),
{
    let mut aggregate_stats = BTreeMap::new();

    for day_stats in daily_stats.values() {
        let (period_key, period_date) = period_key_fn(day_stats);

        let aggregate_entry = aggregate_stats
            .entry(period_key)
            .or_insert_with(|| DailyStats {
                date: period_date,
                ..DailyStats::default()
            });

        *aggregate_entry += day_stats;
    }

    aggregate_stats
}

/// Roll up daily statistics into monthly totals.
///
/// Groups days by `YYYY-MM`, using the first day of the month as the
/// representative `CompactDate` stored in the aggregated row.
pub fn aggregate_daily_stats_by_month(
    daily_stats: &BTreeMap<String, DailyStats>,
) -> BTreeMap<String, DailyStats> {
    aggregate_daily_stats_by_period(daily_stats, |day_stats| {
        let year = day_stats.date.year();
        let month = day_stats.date.month();
        (
            format!("{year:04}-{month:02}"),
            CompactDate::from_parts(year, month, 1),
        )
    })
}

/// Roll up daily statistics into ISO weekly totals.
///
/// Weeks use ISO-8601 semantics, so they start on Monday and are keyed as
/// `YYYY-Www`, where `YYYY` is the ISO week year.
pub fn aggregate_daily_stats_by_week(
    daily_stats: &BTreeMap<String, DailyStats>,
) -> BTreeMap<String, DailyStats> {
    aggregate_daily_stats_by_period(daily_stats, |day_stats| {
        let date = NaiveDate::from_ymd_opt(
            day_stats.date.year() as i32,
            day_stats.date.month() as u32,
            day_stats.date.day() as u32,
        )
        .expect("CompactDate should always contain a valid calendar date");
        let iso_week = date.iso_week();
        let week_start = NaiveDate::from_isoywd_opt(iso_week.year(), iso_week.week(), Weekday::Mon)
            .expect("ISO week from a valid date should map back to a valid Monday");

        (
            format!("{:04}-W{:02}", iso_week.year(), iso_week.week()),
            CompactDate::from_parts(
                week_start.year() as u16,
                week_start.month() as u8,
                week_start.day() as u8,
            ),
        )
    })
}

/// Roll up daily statistics into yearly totals.
///
/// Yearly rows are keyed as `YYYY` and use January 1st as the representative
/// `CompactDate`.
pub fn aggregate_daily_stats_by_year(
    daily_stats: &BTreeMap<String, DailyStats>,
) -> BTreeMap<String, DailyStats> {
    aggregate_daily_stats_by_period(daily_stats, |day_stats| {
        let year = day_stats.date.year();
        (format!("{year:04}"), CompactDate::from_parts(year, 1, 1))
    })
}

/// Return whether a period contains no visible activity for the aggregate table.
pub fn is_empty_period(stats: &DailyStats) -> bool {
    stats.stats.cost_cents == 0
        && stats.stats.cached_tokens == 0
        && stats.stats.input_tokens == 0
        && stats.stats.output_tokens == 0
        && stats.stats.reasoning_tokens == 0
        && stats.conversations == 0
        && stats.user_messages == 0
        && stats.ai_messages == 0
        && stats.stats.tool_calls == 0
}

/// Collect aggregate keys after applying empty-period filtering and sort order.
pub fn filtered_aggregate_keys(
    aggregate_stats: &BTreeMap<String, DailyStats>,
    hide_empty_periods: bool,
    sort_reversed: bool,
) -> Vec<String> {
    let mut keys: Vec<String> = aggregate_stats
        .iter()
        .filter(|(_, stats)| !hide_empty_periods || !is_empty_period(stats))
        .map(|(key, _)| key.clone())
        .collect();

    if sort_reversed {
        keys.reverse();
    }

    keys
}

/// Check if an AnalyzerStatsView has any data to display.
pub fn has_data_view(stats: &crate::types::AnalyzerStatsView) -> bool {
    stats.num_conversations > 0
        || stats.daily_stats.values().any(|day| {
            day.stats.cost_cents > 0
                || day.stats.input_tokens > 0
                || day.stats.output_tokens > 0
                || day.stats.reasoning_tokens > 0
                || day.stats.tool_calls > 0
        })
}

/// Check if a SharedAnalyzerView has any data to display.
/// Acquires a read lock to check the data.
pub fn has_data_shared(stats: &crate::types::SharedAnalyzerView) -> bool {
    has_data_view(&stats.read())
}

/// Aggregate sessions from a slice of messages with a specified analyzer name.
/// Used when converting AgenticCodingToolStats to AnalyzerStatsView.
///
/// Takes `Arc<str>` for analyzer_name to avoid allocating a new String per session.
/// The Arc is cloned (cheap pointer copy) into each SessionAggregate.
pub fn aggregate_sessions_from_messages(
    messages: &[ConversationMessage],
    analyzer_name: Arc<str>,
) -> Vec<SessionAggregate> {
    let mut sessions: BTreeMap<String, SessionAggregate> = BTreeMap::new();

    for msg in messages {
        // Use or_insert_with_key to avoid redundant cloning:
        // - Pass owned key to entry() (1 clone of conversation_hash)
        // - Clone key only when inserting a new session (via closure's &key)
        let entry = sessions
            .entry(msg.conversation_hash.clone())
            .or_insert_with_key(|key| SessionAggregate {
                session_id: key.clone(),
                first_timestamp: msg.date,
                analyzer_name: Arc::clone(&analyzer_name),
                stats: TuiStats::default(),
                models: ModelCounts::new(),
                session_name: None,
                date: CompactDate::from_local(&msg.date),
            });

        if msg.date < entry.first_timestamp {
            entry.first_timestamp = msg.date;
            entry.date = CompactDate::from_local(&msg.date);
        }

        // Only aggregate stats for assistant messages and track models when known.
        if msg.role == MessageRole::Assistant {
            accumulate_tui_stats(&mut entry.stats, &msg.stats);

            if let Some(model) = &msg.model {
                entry.models.increment(intern_model(model), 1);
            }
        }

        // Capture session name if available
        if let Some(name) = &msg.session_name {
            entry.session_name = Some(name.clone());
        }
    }

    let mut result: Vec<SessionAggregate> = sessions.into_values().collect();

    // Sort oldest sessions first so newest appear at the bottom
    result.sort_by_key(|s| s.first_timestamp);

    // Shrink to fit to release excess capacity
    result.shrink_to_fit();

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AnalyzerStatsView;

    #[test]
    fn has_data_view_returns_true_for_non_empty() {
        let view = AnalyzerStatsView {
            daily_stats: BTreeMap::new(),
            session_aggregates: vec![],
            num_conversations: 1,
            analyzer_name: Arc::from("Test"),
        };

        assert!(has_data_view(&view));
    }

    #[test]
    fn has_data_view_returns_false_for_empty() {
        let view = AnalyzerStatsView {
            daily_stats: BTreeMap::new(),
            session_aggregates: vec![],
            num_conversations: 0,
            analyzer_name: Arc::from("Test"),
        };

        assert!(!has_data_view(&view));
    }
}
