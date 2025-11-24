use crate::types::{AgenticCodingToolStats, Stats};
use chrono::{DateTime, Local, Utc};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct SessionAggregate {
    pub session_id: String,
    pub first_timestamp: DateTime<Utc>,
    pub analyzer_name: String,
    pub stats: Stats,
    pub models: Vec<String>,
    pub session_name: Option<String>,
    pub day_key: String,
}

pub fn accumulate_stats(dst: &mut Stats, src: &Stats) {
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

pub fn aggregate_sessions_for_tool(stats: &AgenticCodingToolStats) -> Vec<SessionAggregate> {
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
                day_key: msg
                    .date
                    .with_timezone(&Local)
                    .format("%Y-%m-%d")
                    .to_string(),
            });

        if msg.date < entry.first_timestamp {
            entry.first_timestamp = msg.date;
            entry.day_key = msg
                .date
                .with_timezone(&Local)
                .format("%Y-%m-%d")
                .to_string();
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

pub fn aggregate_sessions_for_all_tools(
    filtered_stats: &[&AgenticCodingToolStats],
) -> Vec<Vec<SessionAggregate>> {
    filtered_stats
        .iter()
        .map(|stats| aggregate_sessions_for_tool(stats))
        .collect()
}

pub fn aggregate_sessions_for_all_tools_owned(
    stats: &[AgenticCodingToolStats],
) -> Vec<Vec<SessionAggregate>> {
    stats.iter().map(aggregate_sessions_for_tool).collect()
}

/// Check if a date string (YYYY-MM-DD format) matches the user's search buffer
pub fn date_matches_buffer(day: &str, buffer: &str) -> bool {
    if buffer.is_empty() {
        return true;
    }

    // Check for month name match first
    let lower = buffer.to_lowercase();
    let month_num = match lower.as_str() {
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
    };

    if let Some(month) = month_num {
        let target = format!("-{:02}-", month);
        return day.contains(&target);
    }

    let normalized_input = buffer.replace('/', "-");

    // Remove trailing separator for partial matches like "7/" or "7-"
    let trimmed = normalized_input.trim_end_matches('-');

    // Exact match
    if day == buffer {
        return true;
    }

    let parts: Vec<&str> = trimmed.split('-').filter(|s| !s.is_empty()).collect();
    if parts.len() == 1 {
        // Single number - match as month
        if let Ok(month) = parts[0].parse::<u32>() {
            let target = format!("-{:02}-", month);
            return day.contains(&target);
        }
        // Otherwise match if the date contains this string
        return day.contains(trimmed);
    } else if parts.len() == 2 {
        // Month and day only (M-D or MM-DD) or Year and Month (YYYY-MM)
        if let (Ok(p1), Ok(p2)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) {
            if p1 > 31 {
                // Assume Year-Month
                let target = format!("{:04}-{:02}", p1, p2);
                return day.starts_with(&target);
            } else {
                // Assume Month-Day
                let target = format!("-{:02}-{:02}", p1, p2);
                return day.ends_with(&target);
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
            let (year, month, day_num) = if p0 > 31 {
                // YYYY-M-D format
                (p0, p1, p2)
            } else if p2 > 31 {
                // M/D/YYYY format
                (p2, p0, p1)
            } else {
                // Ambiguous, assume YYYY-M-D
                (p0, p1, p2)
            };
            let target = format!("{:04}-{:02}-{:02}", year, month, day_num);
            return day == target;
        }
    }

    false
}

pub fn has_data(stats: &AgenticCodingToolStats) -> bool {
    stats.num_conversations > 0
        || stats.daily_stats.values().any(|day| {
            day.stats.cost > 0.0
                || day.stats.input_tokens > 0
                || day.stats.output_tokens > 0
                || day.stats.reasoning_tokens > 0
                || day.stats.tool_calls > 0
        })
}
