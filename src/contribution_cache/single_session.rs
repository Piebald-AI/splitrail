//! Single-session contribution type for 1-file-1-session analyzers.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{Local, Timelike};

use super::SessionHash;
use crate::types::{
    CompactDate, ConversationMessage, MessageRole, ModelCounts, ModelStats, SessionPeriodAggregate,
    TuiStats, intern_model,
};

// ============================================================================
// SingleSessionContribution - For 1 file = 1 session analyzers
// ============================================================================

/// Contribution for single-session-per-file analyzers.
/// Uses ~72 bytes instead of ~100+ bytes for full contributions.
/// Designed for most analyzers where each file contains one conversation/session.
#[derive(Debug, Clone)]
pub struct SingleSessionContribution {
    /// Aggregated stats from all messages in this session
    pub stats: TuiStats,
    /// Primary date (date of first message)
    pub date: CompactDate,
    /// Models used in this session with reference counts
    pub models: ModelCounts,
    /// Stable local project identity used to group moved repositories and worktrees.
    pub project_id: Option<Arc<str>>,
    /// Local project path used to keep incremental TUI updates in the right project scope.
    pub project_path: Option<Arc<str>>,
    /// Hash of conversation_hash for session lookup
    pub session_hash: SessionHash,
    /// Number of AI messages (for daily_stats.ai_messages)
    pub ai_message_count: u32,
    /// Per-day session activity for period drill-down and incremental updates.
    pub daily: BTreeMap<CompactDate, SessionPeriodAggregate>,
    /// Per-hour session activity keyed by local time as `YYYY-MM-DDTHH`.
    pub hourly: BTreeMap<String, SessionPeriodAggregate>,
}

impl SingleSessionContribution {
    /// Create from messages belonging to a single session.
    pub fn from_messages(messages: &[ConversationMessage]) -> Self {
        let mut stats = TuiStats::default();
        let mut models = ModelCounts::new();
        let mut ai_message_count = 0u32;
        let mut first_date = CompactDate::default();
        let mut session_hash = SessionHash::default();
        let mut daily = BTreeMap::new();
        let mut hourly = BTreeMap::new();

        for (i, msg) in messages.iter().enumerate() {
            let date = CompactDate::from_local(&msg.date);
            if i == 0 {
                first_date = date;
                session_hash = SessionHash::from_str(&msg.conversation_hash);
            }

            let day = daily
                .entry(date)
                .or_insert_with(SessionPeriodAggregate::default);
            day.message_count = day.message_count.saturating_add(1);
            let local = msg.date.with_timezone(&Local);
            let hour = hourly
                .entry(format!("{}T{:02}", date, local.hour()))
                .or_insert_with(SessionPeriodAggregate::default);
            hour.message_count = hour.message_count.saturating_add(1);
            if msg.role == MessageRole::Assistant {
                ai_message_count += 1;
                day.ai_message_count = day.ai_message_count.saturating_add(1);
                hour.ai_message_count = hour.ai_message_count.saturating_add(1);
                let message_stats = TuiStats::from(&msg.stats);
                stats += message_stats;
                day.stats += message_stats;
                hour.stats += message_stats;

                if let Some(model) = &msg.model {
                    let model_key = intern_model(model);
                    models.increment(model_key, 1);
                    day.models.increment(model_key, 1);
                    hour.models.increment(model_key, 1);
                    day.model_stats
                        .entry(model.to_string())
                        .or_insert_with(|| ModelStats::new(model.to_string()))
                        .add_message(&msg.stats);
                    hour.model_stats
                        .entry(model.to_string())
                        .or_insert_with(|| ModelStats::new(model.to_string()))
                        .add_message(&msg.stats);
                }
            }
        }

        Self {
            stats,
            date: first_date,
            models,
            project_id: messages
                .iter()
                .find_map(|message| {
                    (!message.project_hash.is_empty()).then_some(message.project_hash.as_str())
                })
                .or_else(|| {
                    messages
                        .iter()
                        .find_map(|message| message.project_path.as_deref())
                })
                .map(Arc::from),
            project_path: messages
                .iter()
                .find_map(|message| message.project_path.as_deref())
                .map(Arc::from),
            session_hash,
            ai_message_count,
            daily,
            hourly,
        }
    }
}
