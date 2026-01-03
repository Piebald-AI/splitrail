//! Tests for MultiSession contribution strategy (Piebald-style: 1 file = many sessions)

use std::path::PathBuf;
use std::sync::Arc;

use super::super::{ContributionCache, MultiSessionContribution, PathHash};
use super::{make_empty_view, make_message};

// ============================================================================
// MultiSessionContribution Tests
// ============================================================================

#[test]
fn test_multi_session_contribution_from_messages() {
    let messages = vec![
        // Session 1
        make_message(
            "session1",
            Some("claude-3-5-sonnet"),
            500,
            200,
            0.02,
            1,
            "2025-01-15",
        ),
        make_message(
            "session1",
            Some("claude-3-5-sonnet"),
            600,
            250,
            0.025,
            2,
            "2025-01-15",
        ),
        // Session 2
        make_message(
            "session2",
            Some("claude-3-opus"),
            800,
            300,
            0.05,
            3,
            "2025-01-16",
        ),
    ];

    let contrib = MultiSessionContribution::from_messages(&messages, Arc::from("TestAnalyzer"));

    // Should have 2 session aggregates
    assert_eq!(contrib.session_aggregates.len(), 2);
    assert_eq!(contrib.conversation_count, 2);

    // Daily stats will have gap-filled entries (from earliest date to today),
    // but should contain our specific dates with non-empty stats
    assert!(contrib.daily_stats.contains_key("2025-01-15"));
    assert!(contrib.daily_stats.contains_key("2025-01-16"));

    // Verify the actual stats for our dates are populated
    let day1 = contrib.daily_stats.get("2025-01-15").unwrap();
    assert_eq!(day1.ai_messages, 2); // Two AI messages on Jan 15
    let day2 = contrib.daily_stats.get("2025-01-16").unwrap();
    assert_eq!(day2.ai_messages, 1); // One AI message on Jan 16
}

#[test]
fn test_multi_session_contribution_empty_messages() {
    let messages: Vec<_> = vec![];

    let contrib = MultiSessionContribution::from_messages(&messages, Arc::from("TestAnalyzer"));

    assert_eq!(contrib.session_aggregates.len(), 0);
    assert_eq!(contrib.conversation_count, 0);
    assert!(contrib.daily_stats.is_empty());
}

// ============================================================================
// AnalyzerStatsView Add/Subtract Tests - MultiSession Strategy
// ============================================================================

#[test]
fn test_view_add_multi_session_contribution() {
    let mut view = make_empty_view("TestAnalyzer");
    let messages = vec![
        make_message(
            "session1",
            Some("claude-3-5-sonnet"),
            500,
            200,
            0.02,
            1,
            "2025-01-15",
        ),
        make_message(
            "session2",
            Some("claude-3-opus"),
            800,
            300,
            0.05,
            3,
            "2025-01-16",
        ),
    ];
    let contrib = MultiSessionContribution::from_messages(&messages, Arc::from("TestAnalyzer"));

    view.add_multi_session_contribution(&contrib);

    // Check conversation count increased
    assert_eq!(view.num_conversations, 2);

    // Check sessions added
    assert_eq!(view.session_aggregates.len(), 2);

    // Check daily stats
    assert!(view.daily_stats.contains_key("2025-01-15"));
    assert!(view.daily_stats.contains_key("2025-01-16"));
}

#[test]
fn test_view_subtract_multi_session_contribution() {
    let mut view = make_empty_view("TestAnalyzer");
    let messages = vec![
        make_message(
            "session1",
            Some("claude-3-5-sonnet"),
            500,
            200,
            0.02,
            1,
            "2025-01-15",
        ),
        make_message(
            "session2",
            Some("claude-3-opus"),
            800,
            300,
            0.05,
            3,
            "2025-01-16",
        ),
    ];
    let contrib = MultiSessionContribution::from_messages(&messages, Arc::from("TestAnalyzer"));

    // Add then subtract
    view.add_multi_session_contribution(&contrib);
    view.subtract_multi_session_contribution(&contrib);

    // Conversation count should be 0
    assert_eq!(view.num_conversations, 0);

    // Daily stats should be removed when empty
    assert!(view.daily_stats.is_empty());
}

#[test]
fn test_view_multi_session_merges_existing_sessions() {
    let mut view = make_empty_view("TestAnalyzer");

    // First contribution with session1
    let messages1 = vec![make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        500,
        200,
        0.02,
        1,
        "2025-01-15",
    )];
    let contrib1 = MultiSessionContribution::from_messages(&messages1, Arc::from("TestAnalyzer"));
    view.add_multi_session_contribution(&contrib1);

    // Second contribution with same session1 (should merge)
    let messages2 = vec![make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        800,
        300,
        0.03,
        2,
        "2025-01-15",
    )];
    let contrib2 = MultiSessionContribution::from_messages(&messages2, Arc::from("TestAnalyzer"));
    view.add_multi_session_contribution(&contrib2);

    // Should still have 1 session (merged)
    assert_eq!(view.session_aggregates.len(), 1);

    // Stats should be combined
    let session = &view.session_aggregates[0];
    assert_eq!(session.stats.input_tokens, 1300); // 500 + 800
}

// ============================================================================
// File Update Simulation Tests - MultiSession Strategy
// ============================================================================

/// Tests the subtract-old/add-new contribution flow for file updates
#[test]
fn test_file_update_flow_multi_session() {
    let cache = ContributionCache::new();
    let path = PathBuf::from("/test/app.db");
    let path_hash = PathHash::new(&path);

    let mut view = make_empty_view("TestAnalyzer");

    // Initial: 1 session
    let messages1 = vec![make_message(
        "session1",
        Some("claude-3-5-sonnet"),
        1000,
        500,
        0.05,
        3,
        "2025-01-15",
    )];
    let contrib1 = MultiSessionContribution::from_messages(&messages1, Arc::from("TestAnalyzer"));

    cache.insert_multi_session(path_hash, contrib1.clone());
    view.add_multi_session_contribution(&contrib1);

    assert_eq!(view.num_conversations, 1);
    assert_eq!(view.session_aggregates.len(), 1);

    // File updated: now 2 sessions
    let messages2 = vec![
        make_message(
            "session1",
            Some("claude-3-5-sonnet"),
            1000,
            500,
            0.05,
            3,
            "2025-01-15",
        ),
        make_message(
            "session2",
            Some("claude-3-opus"),
            2000,
            800,
            0.10,
            5,
            "2025-01-16",
        ),
    ];
    let contrib2 = MultiSessionContribution::from_messages(&messages2, Arc::from("TestAnalyzer"));

    // Subtract old, add new
    let old = cache.get_multi_session(&path_hash).unwrap();
    view.subtract_multi_session_contribution(&old);
    view.add_multi_session_contribution(&contrib2);
    cache.insert_multi_session(path_hash, contrib2);

    // Should have new values
    assert_eq!(view.num_conversations, 2);
    assert_eq!(view.session_aggregates.len(), 2);
    assert!(view.daily_stats.contains_key("2025-01-15"));
    assert!(view.daily_stats.contains_key("2025-01-16"));
}

/// Tests file deletion for MultiSession
#[test]
fn test_file_deletion_multi_session() {
    let cache = ContributionCache::new();
    let path = PathBuf::from("/test/app.db");
    let path_hash = PathHash::new(&path);

    let mut view = make_empty_view("TestAnalyzer");

    // Add file with 2 sessions
    let messages = vec![
        make_message(
            "session1",
            Some("claude-3-5-sonnet"),
            1000,
            500,
            0.05,
            3,
            "2025-01-15",
        ),
        make_message(
            "session2",
            Some("claude-3-opus"),
            2000,
            800,
            0.10,
            5,
            "2025-01-16",
        ),
    ];
    let contrib = MultiSessionContribution::from_messages(&messages, Arc::from("TestAnalyzer"));
    cache.insert_multi_session(path_hash, contrib.clone());
    view.add_multi_session_contribution(&contrib);

    assert_eq!(view.num_conversations, 2);

    // Delete file
    if let Some(super::super::RemovedContribution::MultiSession(old)) = cache.remove_any(&path_hash)
    {
        view.subtract_multi_session_contribution(&old);
    }

    // Stats should be cleared
    assert_eq!(view.num_conversations, 0);
    assert!(view.daily_stats.is_empty());
}
