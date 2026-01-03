//! Contribution caching for incremental updates.
//!
//! Provides memory-efficient caching strategies for different analyzer types:
//! - [`SingleMessageContribution`]: ~40 bytes for 1-message-per-file analyzers (OpenCode)
//! - [`SingleSessionContribution`]: ~72 bytes for 1-session-per-file analyzers (most)
//! - [`MultiSessionContribution`]: ~100+ bytes for all-in-one-file analyzers (Piebald)

mod multi_session;
mod single_message;
mod single_session;

pub use multi_session::MultiSessionContribution;
pub use single_message::SingleMessageContribution;
pub use single_session::SingleSessionContribution;

use std::path::Path;

use dashmap::DashMap;
use xxhash_rust::xxh3::xxh3_64;

use crate::types::{AnalyzerStatsView, CompactDate, DailyStats, TuiStats};

// ============================================================================
// PathHash - Cache key type
// ============================================================================

/// Newtype wrapper for xxh3 path hashes, used as cache keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PathHash(u64);

impl PathHash {
    /// Hash a path using xxh3 for cache key lookup.
    #[inline]
    pub fn new(path: &Path) -> Self {
        Self(xxh3_64(path.as_os_str().as_encoded_bytes()))
    }
}

// ============================================================================
// ContributionStrategy - Analyzer categorization
// ============================================================================

/// Strategy for caching file contributions based on analyzer data structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContributionStrategy {
    /// 1 file = 1 message (e.g., OpenCode)
    /// Uses `SingleMessageContribution` (~40 bytes per file)
    SingleMessage,

    /// 1 file = 1 session = many messages (e.g., Claude Code, Cline, Copilot)
    /// Uses `SingleSessionContribution` (~72 bytes per file)
    SingleSession,

    /// 1 file = many sessions (e.g., Piebald with SQLite)
    /// Uses `MultiSessionContribution` (~100+ bytes per file)
    MultiSession,
}

// ============================================================================
// CompactMessageStats - Ultra-lightweight stats for single messages
// ============================================================================

/// Ultra-compact stats for single-message contributions.
/// Uses u16 for cost (max $655.35 per message) and u8 for tool_calls.
/// Total: 20 bytes (vs 24 bytes for TuiStats)
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CompactMessageStats {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub reasoning_tokens: u32,
    pub cached_tokens: u32,
    /// Cost in cents (max $655.35 per message)
    pub cost_cents: u16,
    /// Tool calls per message (max 255)
    pub tool_calls: u8,
}

impl CompactMessageStats {
    /// Get cost as f64 dollars for display
    #[inline]
    pub fn cost(&self) -> f64 {
        self.cost_cents as f64 / 100.0
    }

    /// Create from full Stats
    #[inline]
    pub fn from_stats(s: &crate::types::Stats) -> Self {
        Self {
            input_tokens: s.input_tokens as u32,
            output_tokens: s.output_tokens as u32,
            reasoning_tokens: s.reasoning_tokens as u32,
            cached_tokens: s.cached_tokens as u32,
            cost_cents: (s.cost * 100.0).round().min(u16::MAX as f64) as u16,
            tool_calls: s.tool_calls.min(u8::MAX as u32) as u8,
        }
    }

    /// Convert to TuiStats for view operations
    #[inline]
    pub fn to_tui_stats(self) -> TuiStats {
        TuiStats {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            reasoning_tokens: self.reasoning_tokens,
            cached_tokens: self.cached_tokens,
            cost_cents: self.cost_cents as u32,
            tool_calls: self.tool_calls as u32,
        }
    }
}

impl std::ops::AddAssign for CompactMessageStats {
    fn add_assign(&mut self, rhs: Self) {
        self.input_tokens = self.input_tokens.saturating_add(rhs.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(rhs.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(rhs.reasoning_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(rhs.cached_tokens);
        self.cost_cents = self.cost_cents.saturating_add(rhs.cost_cents);
        self.tool_calls = self.tool_calls.saturating_add(rhs.tool_calls);
    }
}

impl std::ops::SubAssign for CompactMessageStats {
    fn sub_assign(&mut self, rhs: Self) {
        self.input_tokens = self.input_tokens.saturating_sub(rhs.input_tokens);
        self.output_tokens = self.output_tokens.saturating_sub(rhs.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_sub(rhs.reasoning_tokens);
        self.cached_tokens = self.cached_tokens.saturating_sub(rhs.cached_tokens);
        self.cost_cents = self.cost_cents.saturating_sub(rhs.cost_cents);
        self.tool_calls = self.tool_calls.saturating_sub(rhs.tool_calls);
    }
}

// ============================================================================
// ContributionCache - Unified cache wrapper
// ============================================================================

/// Unified cache for file contributions with strategy-specific storage.
/// Uses three separate DashMaps for type safety and memory efficiency.
pub struct ContributionCache {
    /// Cache for single-message-per-file analyzers (~40 bytes per entry)
    single_message: DashMap<PathHash, SingleMessageContribution>,
    /// Cache for single-session-per-file analyzers (~72 bytes per entry)
    single_session: DashMap<PathHash, SingleSessionContribution>,
    /// Cache for multi-session-per-file analyzers (~100+ bytes per entry)
    multi_session: DashMap<PathHash, MultiSessionContribution>,
}

impl Default for ContributionCache {
    fn default() -> Self {
        Self::new()
    }
}

impl ContributionCache {
    /// Create a new empty contribution cache.
    pub fn new() -> Self {
        Self {
            single_message: DashMap::new(),
            single_session: DashMap::new(),
            multi_session: DashMap::new(),
        }
    }

    /// Clear all caches.
    pub fn clear(&self) {
        self.single_message.clear();
        self.single_session.clear();
        self.multi_session.clear();
    }

    /// Shrink all caches to fit.
    pub fn shrink_to_fit(&self) {
        self.single_message.shrink_to_fit();
        self.single_session.shrink_to_fit();
        self.multi_session.shrink_to_fit();
    }

    // --- Single Message operations ---

    /// Insert a single-message contribution.
    #[inline]
    pub fn insert_single_message(&self, key: PathHash, contrib: SingleMessageContribution) {
        self.single_message.insert(key, contrib);
    }

    /// Get a single-message contribution.
    #[inline]
    pub fn get_single_message(&self, key: &PathHash) -> Option<SingleMessageContribution> {
        self.single_message.get(key).map(|r| *r)
    }

    // --- Single Session operations ---

    /// Insert a single-session contribution.
    #[inline]
    pub fn insert_single_session(&self, key: PathHash, contrib: SingleSessionContribution) {
        self.single_session.insert(key, contrib);
    }

    /// Get a single-session contribution.
    #[inline]
    pub fn get_single_session(&self, key: &PathHash) -> Option<SingleSessionContribution> {
        self.single_session.get(key).map(|r| r.clone())
    }

    // --- Multi Session operations ---

    /// Insert a multi-session contribution.
    #[inline]
    pub fn insert_multi_session(&self, key: PathHash, contrib: MultiSessionContribution) {
        self.multi_session.insert(key, contrib);
    }

    /// Get a multi-session contribution.
    #[inline]
    pub fn get_multi_session(&self, key: &PathHash) -> Option<MultiSessionContribution> {
        self.multi_session.get(key).map(|r| r.clone())
    }

    // --- Strategy-agnostic removal ---

    /// Try to remove a contribution from any cache, returning which type was found.
    /// Returns None if not found in any cache.
    pub fn remove_any(&self, key: &PathHash) -> Option<RemovedContribution> {
        if let Some((_, c)) = self.single_message.remove(key) {
            return Some(RemovedContribution::SingleMessage(c));
        }
        if let Some((_, c)) = self.single_session.remove(key) {
            return Some(RemovedContribution::SingleSession(c));
        }
        if let Some((_, c)) = self.multi_session.remove(key) {
            return Some(RemovedContribution::MultiSession(c));
        }
        None
    }
}

/// Result of removing a contribution from the cache.
pub enum RemovedContribution {
    SingleMessage(SingleMessageContribution),
    SingleSession(SingleSessionContribution),
    MultiSession(MultiSessionContribution),
}

// ============================================================================
// AnalyzerStatsView extensions for contribution operations
// ============================================================================

impl AnalyzerStatsView {
    /// Add a single-message contribution to this view.
    pub fn add_single_message_contribution(&mut self, contrib: &SingleMessageContribution) {
        // Update daily stats
        let date_str = contrib.date.to_string();
        let day_stats = self
            .daily_stats
            .entry(date_str)
            .or_insert_with(|| DailyStats {
                date: contrib.date,
                ..Default::default()
            });

        // Single message contributes to AI message count and stats
        if contrib.model.is_some() {
            day_stats.ai_messages += 1;
            day_stats.stats += contrib.stats.to_tui_stats();
        }

        // Find session by hash and update
        if let Some(existing) = self.session_aggregates.iter_mut().find(|s| {
            SingleMessageContribution::hash_session_id(&s.session_id) == contrib.session_hash
        }) {
            existing.stats += contrib.stats.to_tui_stats();
            if let Some(model) = contrib.model {
                existing.models.increment(model, 1);
            }
        }
        // Note: We don't create new sessions here - they should already exist from initial load.
    }

    /// Subtract a single-message contribution from this view.
    pub fn subtract_single_message_contribution(&mut self, contrib: &SingleMessageContribution) {
        // Update daily stats
        let date_str = contrib.date.to_string();
        if let Some(day_stats) = self.daily_stats.get_mut(&date_str) {
            if contrib.model.is_some() {
                day_stats.ai_messages = day_stats.ai_messages.saturating_sub(1);
                day_stats.stats -= contrib.stats.to_tui_stats();
            }

            // Remove if empty
            if day_stats.user_messages == 0
                && day_stats.ai_messages == 0
                && day_stats.conversations == 0
            {
                self.daily_stats.remove(&date_str);
            }
        }

        // Find session by hash and subtract
        if let Some(existing) = self.session_aggregates.iter_mut().find(|s| {
            SingleMessageContribution::hash_session_id(&s.session_id) == contrib.session_hash
        }) {
            existing.stats -= contrib.stats.to_tui_stats();
            if let Some(model) = contrib.model {
                existing.models.decrement(model, 1);
            }
        }
    }

    /// Add a single-session contribution to this view.
    pub fn add_single_session_contribution(&mut self, contrib: &SingleSessionContribution) {
        // Update daily stats
        let date_str = contrib.date.to_string();
        let day_stats = self
            .daily_stats
            .entry(date_str)
            .or_insert_with(|| DailyStats {
                date: contrib.date,
                ..Default::default()
            });

        day_stats.ai_messages += contrib.ai_message_count;
        day_stats.stats += contrib.stats;

        // Find session by hash and update
        if let Some(existing) = self.session_aggregates.iter_mut().find(|s| {
            SingleMessageContribution::hash_session_id(&s.session_id) == contrib.session_hash
        }) {
            existing.stats += contrib.stats;
            for &(model, count) in contrib.models.iter() {
                existing.models.increment(model, count);
            }
        }
    }

    /// Subtract a single-session contribution from this view.
    pub fn subtract_single_session_contribution(&mut self, contrib: &SingleSessionContribution) {
        // Update daily stats
        let date_str = contrib.date.to_string();
        if let Some(day_stats) = self.daily_stats.get_mut(&date_str) {
            day_stats.ai_messages = day_stats
                .ai_messages
                .saturating_sub(contrib.ai_message_count);
            day_stats.stats -= contrib.stats;

            // Remove if empty
            if day_stats.user_messages == 0
                && day_stats.ai_messages == 0
                && day_stats.conversations == 0
            {
                self.daily_stats.remove(&date_str);
            }
        }

        // Find session by hash and subtract
        if let Some(existing) = self.session_aggregates.iter_mut().find(|s| {
            SingleMessageContribution::hash_session_id(&s.session_id) == contrib.session_hash
        }) {
            existing.stats -= contrib.stats;
            for &(model, count) in contrib.models.iter() {
                existing.models.decrement(model, count);
            }
        }
    }

    /// Add a multi-session contribution to this view.
    pub fn add_multi_session_contribution(&mut self, contrib: &MultiSessionContribution) {
        // Add daily stats
        for (date, day_stats) in &contrib.daily_stats {
            *self
                .daily_stats
                .entry(date.clone())
                .or_insert_with(|| DailyStats {
                    date: CompactDate::from_str(date).unwrap_or_default(),
                    ..Default::default()
                }) += day_stats;
        }

        // Add session aggregates - merge if same session_id exists, otherwise append
        for new_session in &contrib.session_aggregates {
            if let Some(existing) = self
                .session_aggregates
                .iter_mut()
                .find(|s| s.session_id == new_session.session_id)
            {
                // Merge into existing session
                existing.stats += new_session.stats;
                for &(model, count) in new_session.models.iter() {
                    existing.models.increment(model, count);
                }
                if new_session.first_timestamp < existing.first_timestamp {
                    existing.first_timestamp = new_session.first_timestamp;
                    existing.date = new_session.date;
                }
                if existing.session_name.is_none() {
                    existing.session_name = new_session.session_name.clone();
                }
            } else {
                // New session
                self.session_aggregates.push(new_session.clone());
            }
        }

        self.num_conversations += contrib.conversation_count;

        // Keep sessions sorted by timestamp
        self.session_aggregates.sort_by_key(|s| s.first_timestamp);
    }

    /// Subtract a multi-session contribution from this view.
    pub fn subtract_multi_session_contribution(&mut self, contrib: &MultiSessionContribution) {
        // Subtract daily stats
        for (date, day_stats) in &contrib.daily_stats {
            if let Some(existing) = self.daily_stats.get_mut(date) {
                *existing -= day_stats;
                // Remove if empty
                if existing.user_messages == 0
                    && existing.ai_messages == 0
                    && existing.conversations == 0
                {
                    self.daily_stats.remove(date);
                }
            }
        }

        // Subtract session stats
        for old_session in &contrib.session_aggregates {
            if let Some(existing) = self
                .session_aggregates
                .iter_mut()
                .find(|s| s.session_id == old_session.session_id)
            {
                existing.stats -= old_session.stats;
                for &(model, count) in old_session.models.iter() {
                    existing.models.decrement(model, count);
                }
            }
        }

        self.num_conversations = self
            .num_conversations
            .saturating_sub(contrib.conversation_count);
    }
}

#[cfg(test)]
mod tests;
