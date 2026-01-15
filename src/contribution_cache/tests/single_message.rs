//! Tests for SingleMessage contribution strategy (OpenCode-style: 1 file = 1 message)

use std::path::PathBuf;

use super::super::{ContributionCache, PathHash, SingleMessageContribution};
use super::{make_message, make_view_with_session};

// ============================================================================
// SingleMessageContribution Tests
// ============================================================================

#[test]
fn test_single_message_contribution_from_message() {
    let msg = make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        1000,
        500,
        0.05,
        3,
        "2025-01-15",
    );

    let contrib = SingleMessageContribution::from_message(&msg);
    let stats = contrib.to_tui_stats();

    assert_eq!(stats.input_tokens, 1000);
    assert_eq!(stats.output_tokens, 500);
    assert_eq!(stats.cost_cents, 5);
    assert_eq!(stats.tool_calls, 3);
    assert!(contrib.model.is_some());
    assert_eq!(contrib.date().to_string(), "2025-01-15");
}

#[test]
fn test_single_message_contribution_from_user_message() {
    let msg = make_message("session1", None, 0, 0, 0.0, 0, "2025-01-15");

    let contrib = SingleMessageContribution::from_message(&msg);
    let stats = contrib.to_tui_stats();

    assert!(contrib.model.is_none());
    assert_eq!(stats.input_tokens, 0);
}

#[test]
fn test_single_message_hash_session_id_consistency() {
    let session_id = "test_session_123";

    let hash1 = SingleMessageContribution::hash_session_id(session_id);
    let hash2 = SingleMessageContribution::hash_session_id(session_id);

    assert_eq!(hash1, hash2, "Same session ID should produce same hash");

    let hash3 = SingleMessageContribution::hash_session_id("different_session");
    assert_ne!(
        hash1, hash3,
        "Different session IDs should produce different hashes"
    );
}

// ============================================================================
// AnalyzerStatsView Add/Subtract Tests - SingleMessage Strategy
// ============================================================================

#[test]
fn test_view_add_single_message_contribution() {
    let mut view = make_view_with_session("TestAnalyzer", "session1");
    let msg = make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        1000,
        500,
        0.05,
        3,
        "2025-01-15",
    );
    let contrib = SingleMessageContribution::from_message(&msg);

    view.add_single_message_contribution(&contrib);

    // Check daily stats updated
    let daily = view.daily_stats.get("2025-01-15").expect("daily stats");
    assert_eq!(daily.ai_messages, 1);
    assert_eq!(daily.stats.input_tokens, 1000);
    assert_eq!(daily.stats.output_tokens, 500);

    // Check session stats updated
    let session = &view.session_aggregates[0];
    assert_eq!(session.stats.input_tokens, 1000);
    assert_eq!(session.stats.output_tokens, 500);
}

#[test]
fn test_view_subtract_single_message_contribution() {
    let mut view = make_view_with_session("TestAnalyzer", "session1");

    // Add first
    let msg = make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        1000,
        500,
        0.05,
        3,
        "2025-01-15",
    );
    let contrib = SingleMessageContribution::from_message(&msg);
    view.add_single_message_contribution(&contrib);

    // Then subtract
    view.subtract_single_message_contribution(&contrib);

    // Daily stats should be removed when empty
    assert!(
        view.daily_stats.is_empty(),
        "Empty daily stats should be removed"
    );

    // Session stats should be zeroed
    let session = &view.session_aggregates[0];
    assert_eq!(session.stats.input_tokens, 0);
}

#[test]
fn test_view_add_single_message_user_message_no_change() {
    let mut view = make_view_with_session("TestAnalyzer", "session1");
    let msg = make_message("session1", None, 0, 0, 0.0, 0, "2025-01-15"); // User message
    let contrib = SingleMessageContribution::from_message(&msg);

    view.add_single_message_contribution(&contrib);

    // User messages (model=None) should not increment ai_messages
    assert!(
        view.daily_stats.is_empty() || view.daily_stats.get("2025-01-15").unwrap().ai_messages == 0
    );
}

// ============================================================================
// File Update Simulation Tests - SingleMessage Strategy
// ============================================================================

/// Tests the subtract-old/add-new contribution flow for file updates
#[test]
fn test_file_update_flow_single_message() {
    let cache = ContributionCache::new();
    let path = PathBuf::from("/test/message1.json");
    let path_hash = PathHash::new(&path);

    let mut view = make_view_with_session("TestAnalyzer", "session1");

    // Initial file: 1000 input tokens
    let msg1 = make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        1000,
        500,
        0.05,
        3,
        "2025-01-15",
    );
    let contrib1 = SingleMessageContribution::from_message(&msg1);

    cache.insert_single_message(path_hash, contrib1);
    view.add_single_message_contribution(&contrib1);

    assert_eq!(view.session_aggregates[0].stats.input_tokens, 1000);

    // File updated: now 2000 input tokens
    let msg2 = make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        2000,
        800,
        0.08,
        5,
        "2025-01-15",
    );
    let contrib2 = SingleMessageContribution::from_message(&msg2);

    // Subtract old, add new (simulating reload_file_incremental)
    let old = cache.get_single_message(&path_hash).unwrap();
    view.subtract_single_message_contribution(&old);
    view.add_single_message_contribution(&contrib2);
    cache.insert_single_message(path_hash, contrib2);

    // Should have new values
    assert_eq!(view.session_aggregates[0].stats.input_tokens, 2000);
    assert_eq!(view.session_aggregates[0].stats.output_tokens, 800);
}

/// Tests file deletion correctly removes contribution
#[test]
fn test_file_deletion_single_message() {
    let cache = ContributionCache::new();
    let path = PathBuf::from("/test/message1.json");
    let path_hash = PathHash::new(&path);

    let mut view = make_view_with_session("TestAnalyzer", "session1");

    // Add file
    let msg = make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        1000,
        500,
        0.05,
        3,
        "2025-01-15",
    );
    let contrib = SingleMessageContribution::from_message(&msg);
    cache.insert_single_message(path_hash, contrib);
    view.add_single_message_contribution(&contrib);

    assert_eq!(view.session_aggregates[0].stats.input_tokens, 1000);

    // Delete file (simulating remove_file_from_cache)
    if let Some(super::super::RemovedContribution::SingleMessage(old)) =
        cache.remove_any(&path_hash)
    {
        view.subtract_single_message_contribution(&old);
    }

    // Stats should be zeroed
    assert_eq!(view.session_aggregates[0].stats.input_tokens, 0);
    assert!(view.daily_stats.is_empty());
}

/// Tests multiple files contributing to the same session
#[test]
fn test_multiple_files_same_session() {
    let cache = ContributionCache::new();
    let mut view = make_view_with_session("TestAnalyzer", "session1");

    // Two files contributing to the same session (SingleMessage strategy)
    let path1 = PathBuf::from("/test/msg1.json");
    let path2 = PathBuf::from("/test/msg2.json");
    let hash1 = PathHash::new(&path1);
    let hash2 = PathHash::new(&path2);

    let msg1 = make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        1000,
        500,
        0.05,
        2,
        "2025-01-15",
    );
    let msg2 = make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        800,
        400,
        0.04,
        3,
        "2025-01-15",
    );

    let contrib1 = SingleMessageContribution::from_message(&msg1);
    let contrib2 = SingleMessageContribution::from_message(&msg2);

    cache.insert_single_message(hash1, contrib1);
    cache.insert_single_message(hash2, contrib2);
    view.add_single_message_contribution(&contrib1);
    view.add_single_message_contribution(&contrib2);

    // Session should have combined stats
    assert_eq!(view.session_aggregates[0].stats.input_tokens, 1800);
    assert_eq!(view.session_aggregates[0].stats.tool_calls, 5);

    // Delete one file
    if let Some(super::super::RemovedContribution::SingleMessage(old)) = cache.remove_any(&hash1) {
        view.subtract_single_message_contribution(&old);
    }

    // Should still have stats from remaining file
    assert_eq!(view.session_aggregates[0].stats.input_tokens, 800);
    assert_eq!(view.session_aggregates[0].stats.tool_calls, 3);
}
