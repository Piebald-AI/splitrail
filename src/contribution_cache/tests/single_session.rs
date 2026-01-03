//! Tests for SingleSession contribution strategy (Claude Code-style: 1 file = 1 session with many messages)

use std::path::PathBuf;

use super::super::{ContributionCache, PathHash, SingleSessionContribution};
use super::{make_message, make_view_with_session};

// ============================================================================
// SingleSessionContribution Tests
// ============================================================================

#[test]
fn test_single_session_contribution_from_messages() {
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
        make_message("session1", None, 0, 0, 0.0, 0, "2025-01-15"), // User message
        make_message(
            "session1",
            Some("claude-3-5-sonnet"),
            800,
            300,
            0.03,
            2,
            "2025-01-15",
        ),
    ];

    let contrib = SingleSessionContribution::from_messages(&messages);

    // Should aggregate only AI messages (2 of them)
    assert_eq!(contrib.ai_message_count, 2);
    assert_eq!(contrib.stats.input_tokens, 1300); // 500 + 800
    assert_eq!(contrib.stats.output_tokens, 500); // 200 + 300
    assert_eq!(contrib.stats.cost_cents, 5); // 2 + 3
    assert_eq!(contrib.stats.tool_calls, 3); // 1 + 2
    assert_eq!(contrib.date.to_string(), "2025-01-15");
}

#[test]
fn test_single_session_contribution_empty_messages() {
    let messages: Vec<_> = vec![];

    let contrib = SingleSessionContribution::from_messages(&messages);

    assert_eq!(contrib.ai_message_count, 0);
    assert_eq!(contrib.stats.input_tokens, 0);
    assert_eq!(contrib.session_hash, 0);
}

#[test]
fn test_single_session_contribution_multiple_models() {
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
            "session1",
            Some("claude-3-opus"),
            800,
            300,
            0.05,
            2,
            "2025-01-15",
        ),
        make_message(
            "session1",
            Some("claude-3-5-sonnet"),
            600,
            250,
            0.025,
            1,
            "2025-01-15",
        ),
    ];

    let contrib = SingleSessionContribution::from_messages(&messages);

    assert_eq!(contrib.ai_message_count, 3);
    // Models should be tracked with counts
    // claude-3-5-sonnet appears twice, claude-3-opus once
    let sonnet_key = crate::types::intern_model("claude-3-5-sonnet");
    let opus_key = crate::types::intern_model("claude-3-opus");

    assert_eq!(contrib.models.get(sonnet_key), Some(2));
    assert_eq!(contrib.models.get(opus_key), Some(1));
}

// ============================================================================
// AnalyzerStatsView Add/Subtract Tests - SingleSession Strategy
// ============================================================================

#[test]
fn test_view_add_single_session_contribution() {
    let mut view = make_view_with_session("TestAnalyzer", "session1");
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
            "session1",
            Some("claude-3-5-sonnet"),
            800,
            300,
            0.03,
            2,
            "2025-01-15",
        ),
    ];
    let contrib = SingleSessionContribution::from_messages(&messages);

    view.add_single_session_contribution(&contrib);

    // Check daily stats
    let daily = view.daily_stats.get("2025-01-15").expect("daily stats");
    assert_eq!(daily.ai_messages, 2);
    assert_eq!(daily.stats.input_tokens, 1300);

    // Check session stats
    let session = &view.session_aggregates[0];
    assert_eq!(session.stats.input_tokens, 1300);
}

#[test]
fn test_view_subtract_single_session_contribution() {
    let mut view = make_view_with_session("TestAnalyzer", "session1");
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
            "session1",
            Some("claude-3-5-sonnet"),
            800,
            300,
            0.03,
            2,
            "2025-01-15",
        ),
    ];
    let contrib = SingleSessionContribution::from_messages(&messages);

    // Add then subtract
    view.add_single_session_contribution(&contrib);
    view.subtract_single_session_contribution(&contrib);

    // Daily stats should be removed
    assert!(view.daily_stats.is_empty());

    // Session stats should be zeroed
    let session = &view.session_aggregates[0];
    assert_eq!(session.stats.input_tokens, 0);
}

#[test]
fn test_view_single_session_model_count_tracking() {
    let mut view = make_view_with_session("TestAnalyzer", "session1");
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
            "session1",
            Some("claude-3-opus"),
            800,
            300,
            0.05,
            2,
            "2025-01-15",
        ),
    ];
    let contrib = SingleSessionContribution::from_messages(&messages);

    view.add_single_session_contribution(&contrib);

    // Check model counts in session
    let session = &view.session_aggregates[0];
    let sonnet_key = crate::types::intern_model("claude-3-5-sonnet");
    let opus_key = crate::types::intern_model("claude-3-opus");

    assert_eq!(session.models.get(sonnet_key), Some(1));
    assert_eq!(session.models.get(opus_key), Some(1));

    // Subtract and verify counts go to zero
    view.subtract_single_session_contribution(&contrib);
    let session = &view.session_aggregates[0];
    assert_eq!(session.models.get(sonnet_key), None); // Removed when count=0
    assert_eq!(session.models.get(opus_key), None);
}

// ============================================================================
// File Update Simulation Tests - SingleSession Strategy
// ============================================================================

/// Tests the subtract-old/add-new contribution flow for file updates
#[test]
fn test_file_update_flow_single_session() {
    let cache = ContributionCache::new();
    let path = PathBuf::from("/test/session1.jsonl");
    let path_hash = PathHash::new(&path);

    let mut view = make_view_with_session("TestAnalyzer", "session1");

    // Initial file: 2 messages
    let messages1 = vec![
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
            500,
            200,
            0.02,
            1,
            "2025-01-15",
        ),
    ];
    let contrib1 = SingleSessionContribution::from_messages(&messages1);

    cache.insert_single_session(path_hash, contrib1.clone());
    view.add_single_session_contribution(&contrib1);

    assert_eq!(view.session_aggregates[0].stats.input_tokens, 1000);
    assert_eq!(view.daily_stats.get("2025-01-15").unwrap().ai_messages, 2);

    // File updated: now 3 messages with different totals
    let messages2 = vec![
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
            500,
            200,
            0.02,
            1,
            "2025-01-15",
        ),
        make_message(
            "session1",
            Some("claude-3-opus"),
            1000,
            400,
            0.05,
            3,
            "2025-01-15",
        ),
    ];
    let contrib2 = SingleSessionContribution::from_messages(&messages2);

    // Subtract old, add new
    let old = cache.get_single_session(&path_hash).unwrap();
    view.subtract_single_session_contribution(&old);
    view.add_single_session_contribution(&contrib2);
    cache.insert_single_session(path_hash, contrib2);

    // Should have new values
    assert_eq!(view.session_aggregates[0].stats.input_tokens, 2000);
    assert_eq!(view.daily_stats.get("2025-01-15").unwrap().ai_messages, 3);
}

/// Tests file deletion for SingleSession
#[test]
fn test_file_deletion_single_session() {
    let cache = ContributionCache::new();
    let path = PathBuf::from("/test/session1.jsonl");
    let path_hash = PathHash::new(&path);

    let mut view = make_view_with_session("TestAnalyzer", "session1");

    // Add file
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
            "session1",
            Some("claude-3-5-sonnet"),
            500,
            200,
            0.02,
            1,
            "2025-01-15",
        ),
    ];
    let contrib = SingleSessionContribution::from_messages(&messages);
    cache.insert_single_session(path_hash, contrib.clone());
    view.add_single_session_contribution(&contrib);

    assert_eq!(view.daily_stats.get("2025-01-15").unwrap().ai_messages, 2);

    // Delete file
    if let Some(super::super::RemovedContribution::SingleSession(old)) =
        cache.remove_any(&path_hash)
    {
        view.subtract_single_session_contribution(&old);
    }

    // Stats should be cleared
    assert!(view.daily_stats.is_empty());
    assert_eq!(view.session_aggregates[0].stats.input_tokens, 0);
}

/// Tests that messages spanning multiple dates are handled correctly
#[test]
fn test_date_boundary_handling() {
    let mut view = make_view_with_session("TestAnalyzer", "session1");

    // Messages on different dates in same session
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
            "session1",
            Some("claude-3-5-sonnet"),
            800,
            300,
            0.03,
            2,
            "2025-01-16",
        ),
    ];
    let contrib = SingleSessionContribution::from_messages(&messages);

    view.add_single_session_contribution(&contrib);

    // Daily stats use the first message's date for SingleSession
    assert!(view.daily_stats.contains_key("2025-01-15"));
    // Second message's date is not separately tracked in SingleSession strategy
}
