//! Single-session contribution type for 1-file-1-session analyzers.

use super::SessionHash;
use crate::types::{CompactDate, ConversationMessage, ModelCounts, TuiStats, intern_model};

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
    /// Hash of conversation_hash for session lookup
    pub session_hash: SessionHash,
    /// Number of AI messages (for daily_stats.ai_messages)
    pub ai_message_count: u32,
}

impl SingleSessionContribution {
    /// Create from messages belonging to a single session.
    pub fn from_messages(messages: &[ConversationMessage]) -> Self {
        let mut stats = TuiStats::default();
        let mut models = ModelCounts::new();
        let mut ai_message_count = 0u32;
        let mut first_date = CompactDate::default();
        let mut session_hash = SessionHash::default();

        for (i, msg) in messages.iter().enumerate() {
            if i == 0 {
                first_date = CompactDate::from_local(&msg.date);
                session_hash = SessionHash::from_str(&msg.conversation_hash);
            }

            if let Some(model) = &msg.model {
                ai_message_count += 1;
                models.increment(intern_model(model), 1);
                stats += TuiStats::from(&msg.stats);
            }
        }

        Self {
            stats,
            date: first_date,
            models,
            session_hash,
            ai_message_count,
        }
    }
}
