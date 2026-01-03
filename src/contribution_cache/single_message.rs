//! Single-message contribution type for 1-file-1-message analyzers.

use super::{CompactMessageStats, SessionHash};
use crate::cache::ModelKey;
use crate::types::{CompactDate, ConversationMessage, intern_model};

// ============================================================================
// SingleMessageContribution - For 1 file = 1 message analyzers
// ============================================================================

/// Lightweight contribution for single-message-per-file analyzers.
/// Uses ~40 bytes instead of ~100+ bytes for full contributions.
/// Designed for analyzers like OpenCode where each file contains exactly one message.
#[derive(Debug, Clone, Copy)]
pub struct SingleMessageContribution {
    /// Compact stats from this file's single message
    pub stats: CompactMessageStats,
    /// Date of the message (for daily_stats updates)
    pub date: CompactDate,
    /// Model used (interned key), None if no model specified
    pub model: Option<ModelKey>,
    /// Hash of conversation_hash for session lookup (avoids String allocation)
    pub session_hash: SessionHash,
}

impl SingleMessageContribution {
    /// Create from a single message.
    #[inline]
    pub fn from_message(msg: &ConversationMessage) -> Self {
        Self {
            stats: CompactMessageStats::from_stats(&msg.stats),
            date: CompactDate::from_local(&msg.date),
            model: msg.model.as_ref().map(|m| intern_model(m)),
            session_hash: SessionHash::from_str(&msg.conversation_hash),
        }
    }

    /// Hash a session_id string for comparison with stored session_hash.
    #[inline]
    pub fn hash_session_id(session_id: &str) -> SessionHash {
        SessionHash::from_str(session_id)
    }
}
