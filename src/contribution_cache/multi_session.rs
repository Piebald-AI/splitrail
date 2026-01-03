//! Multi-session contribution type for all-in-one-file analyzers.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::tui::logic::aggregate_sessions_from_messages;
use crate::types::{ConversationMessage, DailyStats, SessionAggregate};
use crate::utils::aggregate_by_date;

// ============================================================================
// MultiSessionContribution - For all-in-one-file analyzers
// ============================================================================

/// Full contribution for multi-session-per-file analyzers.
/// Used when a single file contains multiple conversations (e.g., Piebald SQLite).
#[derive(Debug, Clone, Default)]
pub struct MultiSessionContribution {
    /// Session aggregates from this file
    pub session_aggregates: Vec<SessionAggregate>,
    /// Daily stats from this file keyed by date
    pub daily_stats: BTreeMap<String, DailyStats>,
    /// Number of conversations in this file
    pub conversation_count: u64,
}

impl MultiSessionContribution {
    /// Compute from parsed messages.
    /// Takes `Arc<str>` for analyzer_name to avoid allocating a new String per session.
    pub fn from_messages(messages: &[ConversationMessage], analyzer_name: Arc<str>) -> Self {
        let session_aggregates = aggregate_sessions_from_messages(messages, analyzer_name);
        let mut daily_stats = aggregate_by_date(messages);
        daily_stats.retain(|date, _| date != "unknown");

        let conversation_count = session_aggregates.len() as u64;

        Self {
            session_aggregates,
            daily_stats,
            conversation_count,
        }
    }
}
