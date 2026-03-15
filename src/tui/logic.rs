/// Logic module for TUI data processing and aggregation.
///
/// Provides functions to aggregate statistics, filter dates, and check for data presence.
use crate::types::{
    CompactDate, ConversationMessage, DailyStats, ModelCounts, Stats, TuiStats, intern_model,
};
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

    let Some((day_year, day_month, day_number)) = parse_period_parts(day) else {
        return day == buffer;
    };

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

/// Roll up daily statistics into monthly totals.
///
/// Groups daily stats by year-month (YYYY-MM) and sums all metrics. Each month
/// entry uses day 1 of that month as its representative date. Returns a new
/// map with monthly aggregated data.
pub fn aggregate_daily_stats_by_month(
    daily_stats: &BTreeMap<String, DailyStats>,
) -> BTreeMap<String, DailyStats> {
    let mut monthly_stats = BTreeMap::new();

    for day_stats in daily_stats.values() {
        let year = day_stats.date.year();
        let month = day_stats.date.month();
        let month_key = format!("{year:04}-{month:02}");

        let monthly_entry = monthly_stats
            .entry(month_key)
            .or_insert_with(|| DailyStats {
                date: CompactDate::from_parts(year, month, 1),
                ..DailyStats::default()
            });

        *monthly_entry += day_stats;
    }

    monthly_stats
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

        // Only aggregate stats for assistant/model messages and track models
        if let Some(model) = &msg.model {
            entry.models.increment(intern_model(model), 1);
            accumulate_tui_stats(&mut entry.stats, &msg.stats);
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
