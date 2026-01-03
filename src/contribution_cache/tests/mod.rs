//! Integration tests for contribution cache strategies.
//!
//! Tests the full flow of file updates for each contribution strategy:
//! - SingleMessage (OpenCode-style: 1 file = 1 message)
//! - SingleSession (Claude Code-style: 1 file = 1 session with many messages)
//! - MultiSession (Piebald-style: 1 file = many sessions)

mod basic_operations;
mod compact_stats;
mod multi_session;
mod single_message;
mod single_session;

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{TimeZone, Utc};

use crate::types::{
    AnalyzerStatsView, Application, CompactDate, ConversationMessage, MessageRole,
    SessionAggregate, Stats, TuiStats,
};

// ============================================================================
// Test Helpers
// ============================================================================

/// Create a test message with configurable parameters.
pub fn make_message(
    session_id: &str,
    model: Option<&str>,
    input_tokens: u64,
    output_tokens: u64,
    cost: f64,
    tool_calls: u32,
    date_str: &str,
) -> ConversationMessage {
    let date = chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .map(|d| {
            d.and_hms_opt(12, 0, 0)
                .map(|dt| Utc.from_utc_datetime(&dt))
                .unwrap_or_else(|| Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap())
        })
        .unwrap_or_else(|_| Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap());

    ConversationMessage {
        application: Application::ClaudeCode,
        date,
        project_hash: "test_project".into(),
        conversation_hash: session_id.into(),
        local_hash: Some(format!("local_{}", session_id)),
        global_hash: format!("global_{}_{}", session_id, input_tokens),
        model: model.map(String::from),
        stats: Stats {
            input_tokens,
            output_tokens,
            cost,
            tool_calls,
            ..Default::default()
        },
        role: if model.is_some() {
            MessageRole::Assistant
        } else {
            MessageRole::User
        },
        uuid: None,
        session_name: Some(format!("Session {}", session_id)),
    }
}

/// Create a minimal AnalyzerStatsView for testing.
pub fn make_empty_view(analyzer_name: &str) -> AnalyzerStatsView {
    AnalyzerStatsView {
        daily_stats: BTreeMap::new(),
        session_aggregates: Vec::new(),
        num_conversations: 0,
        analyzer_name: Arc::from(analyzer_name),
    }
}

/// Create a view with a pre-existing session for testing add operations.
pub fn make_view_with_session(analyzer_name: &str, session_id: &str) -> AnalyzerStatsView {
    let analyzer_name: Arc<str> = Arc::from(analyzer_name);
    AnalyzerStatsView {
        daily_stats: BTreeMap::new(),
        session_aggregates: vec![SessionAggregate {
            session_id: session_id.to_string(),
            first_timestamp: Utc.with_ymd_and_hms(2025, 1, 1, 0, 0, 0).unwrap(),
            analyzer_name: Arc::clone(&analyzer_name),
            stats: TuiStats::default(),
            models: crate::types::ModelCounts::new(),
            session_name: Some(format!("Session {}", session_id)),
            date: CompactDate::from_str("2025-01-01").unwrap(),
        }],
        num_conversations: 0,
        analyzer_name,
    }
}
