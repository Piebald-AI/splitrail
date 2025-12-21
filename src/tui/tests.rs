use crate::tui::logic::*;
use crate::tui::{
    StatsViewMode, UploadStatus, create_upload_progress_callback, run_app_for_tests,
    show_upload_error, show_upload_success, update_day_filters, update_table_states,
    update_window_offsets,
};
use crate::types::{
    AgenticCodingToolStats, Application, ConversationMessage, DailyStats, MessageRole,
    MultiAnalyzerStats, Stats,
};
use chrono::{TimeZone, Utc};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::widgets::TableState;
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use tokio::runtime::Builder;
use tokio::sync::{mpsc, watch};

use crate::utils::NumberFormatOptions;
use crate::watcher::FileWatcher;

// ============================================================================
// TABLE STATE MANAGEMENT TESTS (tui.rs helpers)
// ============================================================================

fn make_tool_stats(name: &str, has_data: bool) -> AgenticCodingToolStats {
    let mut daily_stats = BTreeMap::new();
    if has_data {
        daily_stats.insert(
            "2025-01-01".to_string(),
            crate::types::DailyStats {
                date: "2025-01-01".to_string(),
                user_messages: 0,
                ai_messages: 1,
                conversations: 1,
                models: BTreeMap::new(),
                stats: Stats {
                    input_tokens: 10,
                    ..Stats::default()
                },
            },
        );
    }

    AgenticCodingToolStats {
        daily_stats,
        num_conversations: if has_data { 1 } else { 0 },
        messages: vec![],
        analyzer_name: name.to_string(),
    }
}

fn make_multi_two_tools() -> MultiAnalyzerStats {
    let tool_a = make_tool_stats("Tool A", true);
    let tool_b = make_tool_stats("Tool B", true);
    MultiAnalyzerStats {
        analyzer_stats: vec![tool_a, tool_b],
    }
}

fn make_multi_single_tool_two_days() -> MultiAnalyzerStats {
    let mut daily_stats = BTreeMap::new();
    daily_stats.insert(
        "2025-01-01".to_string(),
        DailyStats {
            date: "2025-01-01".to_string(),
            user_messages: 0,
            ai_messages: 1,
            conversations: 1,
            models: BTreeMap::new(),
            stats: Stats {
                input_tokens: 10,
                ..Stats::default()
            },
        },
    );
    daily_stats.insert(
        "2025-02-01".to_string(),
        DailyStats {
            date: "2025-02-01".to_string(),
            user_messages: 0,
            ai_messages: 1,
            conversations: 1,
            models: BTreeMap::new(),
            stats: Stats {
                input_tokens: 20,
                ..Stats::default()
            },
        },
    );

    let tool = AgenticCodingToolStats {
        daily_stats,
        num_conversations: 2,
        messages: vec![],
        analyzer_name: "Tool A".to_string(),
    };

    MultiAnalyzerStats {
        analyzer_stats: vec![tool],
    }
}

fn run_tui_with_events(
    stats: MultiAnalyzerStats,
    events: Vec<Event>,
    max_iterations: usize,
) -> (usize, StatsViewMode) {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("terminal");

    let (_tx, rx) = watch::channel(stats);

    let format_options = NumberFormatOptions {
        use_comma: false,
        use_human: false,
        locale: "en".to_string(),
        decimal_places: 2,
    };

    let mut selected_tab = 0usize;
    let mut scroll_offset = 0usize;
    let mut stats_view_mode = StatsViewMode::Daily;
    let upload_status = Arc::new(Mutex::new(UploadStatus::None));
    let update_status = Arc::new(Mutex::new(crate::version_check::UpdateStatus::UpToDate));
    let file_watcher = FileWatcher::for_tests();
    let (watcher_tx, _watcher_rx) = mpsc::unbounded_channel();

    let event_queue: std::cell::RefCell<VecDeque<Event>> =
        std::cell::RefCell::new(VecDeque::from(events));

    let rt = Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");

    rt.block_on(async {
        run_app_for_tests(
            &mut terminal,
            rx,
            &format_options,
            &mut selected_tab,
            &mut scroll_offset,
            &mut stats_view_mode,
            upload_status,
            update_status,
            file_watcher,
            watcher_tx,
            |_: std::time::Duration| Ok(!event_queue.borrow().is_empty()),
            || {
                event_queue.borrow_mut().pop_front().ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "no event")
                })
            },
            max_iterations,
        )
        .await
        .expect("run_app_for_tests ok");
    });

    (selected_tab, stats_view_mode)
}

#[test]
fn test_update_table_states_filters_and_preserves_selection() {
    let stats_with_data = make_tool_stats("with-data", true);
    let stats_without_data = make_tool_stats("without-data", false);

    let multi = MultiAnalyzerStats {
        analyzer_stats: vec![stats_with_data, stats_without_data],
    };

    let mut table_states: Vec<TableState> = Vec::new();
    let mut selected_tab = 0usize;

    update_table_states(&mut table_states, &multi, &mut selected_tab);

    // Only analyzers with data should be represented.
    assert_eq!(table_states.len(), 1);
    assert_eq!(selected_tab, 0);
    assert_eq!(table_states[0].selected(), Some(0));

    // If selected_tab is out of range, it should be clamped.
    let mut table_states = vec![TableState::default(); 1];
    let mut selected_tab = 10usize;
    update_table_states(&mut table_states, &multi, &mut selected_tab);
    assert_eq!(selected_tab, 0);
}

#[test]
fn test_update_window_offsets_and_day_filters_resize() {
    let mut offsets = vec![5usize];
    let mut filters: Vec<Option<String>> = vec![Some("2025-01-01".to_string())];

    let count_two = 2usize;
    update_window_offsets(&mut offsets, &count_two);
    update_day_filters(&mut filters, &count_two);

    assert_eq!(offsets, vec![5, 0]);
    assert_eq!(filters, vec![Some("2025-01-01".to_string()), None]);

    let count_one = 1usize;
    update_window_offsets(&mut offsets, &count_one);
    update_day_filters(&mut filters, &count_one);

    assert_eq!(offsets, vec![5]);
    assert_eq!(filters, vec![Some("2025-01-01".to_string())]);
}

// ============================================================================
// UPLOAD PROGRESS & MESSAGES (tui.rs helpers)
// ============================================================================

#[test]
fn test_upload_progress_callback_runs_without_panicking() {
    let format_options = crate::utils::NumberFormatOptions {
        use_comma: false,
        use_human: false,
        locale: "en".to_string(),
        decimal_places: 2,
    };

    let progress = create_upload_progress_callback(&format_options);
    // First call should trigger dots update based on the timestamp.
    progress(0, 10);
    // Second call with changed progress should update even if not enough time has passed.
    progress(5, 10);
}

#[test]
fn test_show_upload_success_and_error_do_not_panic() {
    let format_options = crate::utils::NumberFormatOptions {
        use_comma: true,
        use_human: false,
        locale: "en".to_string(),
        decimal_places: 2,
    };

    show_upload_success(42, &format_options);
    show_upload_error("something went wrong");
}

// ============================================================================
// DATE MATCHING TESTS
// ============================================================================

#[test]
fn test_date_matches_buffer_exact_match() {
    assert!(date_matches_buffer("2025-11-20", "2025-11-20"));
    assert!(date_matches_buffer("2024-01-01", "2024-01-01"));
}

#[test]
fn test_date_matches_buffer_month_names_abbreviated() {
    // Test all month abbreviations
    assert!(date_matches_buffer("2025-01-20", "jan"));
    assert!(date_matches_buffer("2025-02-20", "feb"));
    assert!(date_matches_buffer("2025-03-20", "mar"));
    assert!(date_matches_buffer("2025-04-20", "apr"));
    assert!(date_matches_buffer("2025-05-20", "may"));
    assert!(date_matches_buffer("2025-06-20", "jun"));
    assert!(date_matches_buffer("2025-07-20", "jul"));
    assert!(date_matches_buffer("2025-08-20", "aug"));
    assert!(date_matches_buffer("2025-09-20", "sep"));
    assert!(date_matches_buffer("2025-10-20", "oct"));
    assert!(date_matches_buffer("2025-11-20", "nov"));
    assert!(date_matches_buffer("2025-12-20", "dec"));
}

#[test]
fn test_date_matches_buffer_month_names_full() {
    assert!(date_matches_buffer("2025-11-20", "November"));
    assert!(date_matches_buffer("2025-11-20", "november"));
    assert!(date_matches_buffer("2025-03-15", "March"));
}

#[test]
fn test_date_matches_buffer_partial_numeric() {
    assert!(date_matches_buffer("2025-11-20", "11-20"));
    assert!(date_matches_buffer("2025-11-20", "2025-11"));
    assert!(date_matches_buffer("2025-03-05", "3-5"));
    assert!(date_matches_buffer("2025-12-01", "12-1"));
}

#[test]
fn test_date_matches_buffer_slash_format() {
    assert!(date_matches_buffer("2025-11-20", "11/20"));
    assert!(date_matches_buffer("2025-03-05", "3/5"));
    assert!(date_matches_buffer("2025-12-25", "12/25"));
}

#[test]
fn test_date_matches_buffer_single_month_number() {
    assert!(date_matches_buffer("2025-11-20", "11"));
    assert!(date_matches_buffer("2025-03-15", "3"));
    assert!(date_matches_buffer("2025-01-01", "1"));
}

#[test]
fn test_date_matches_buffer_no_match() {
    assert!(!date_matches_buffer("2025-11-20", "dec"));
    assert!(!date_matches_buffer("2025-11-20", "2024"));
    assert!(!date_matches_buffer("2025-11-20", "12-20"));
    assert!(!date_matches_buffer("2025-11-20", "10-20"));
}

#[test]
fn test_date_matches_buffer_empty_buffer() {
    // Empty buffer should match everything
    assert!(date_matches_buffer("2025-11-20", ""));
    assert!(date_matches_buffer("2024-01-01", ""));
}

#[test]
fn test_date_matches_buffer_month_day_year_format() {
    // M/D/YYYY format
    assert!(date_matches_buffer("2025-11-20", "11/20/2025"));
    assert!(date_matches_buffer("2025-03-05", "3/5/2025"));
}

// ============================================================================
// STATS ACCUMULATION TESTS
// ============================================================================

#[test]
fn test_accumulate_stats_basic() {
    let mut dst = Stats::default();
    let src = Stats {
        input_tokens: 100,
        output_tokens: 50,
        cost: 0.01,
        ..Stats::default()
    };

    accumulate_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 100);
    assert_eq!(dst.output_tokens, 50);
    assert_eq!(dst.cost, 0.01);
}

#[test]
fn test_accumulate_stats_multiple_times() {
    let mut dst = Stats::default();
    let src = Stats {
        input_tokens: 100,
        output_tokens: 50,
        cost: 0.01,
        ..Stats::default()
    };

    accumulate_stats(&mut dst, &src);
    accumulate_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 200);
    assert_eq!(dst.output_tokens, 100);
    assert_eq!(dst.cost, 0.02);
}

#[test]
fn test_accumulate_stats_comprehensive() {
    let mut dst = Stats::default();
    let src = Stats {
        input_tokens: 100,
        output_tokens: 50,
        reasoning_tokens: 25,
        cache_creation_tokens: 10,
        cache_read_tokens: 5,
        cached_tokens: 15,
        cost: 0.01,
        tool_calls: 3,
        terminal_commands: 2,
        file_searches: 1,
        files_read: 5,
        files_edited: 2,
        lines_added: 100,
        lines_deleted: 50,
        bytes_added: 5000,
        ..Stats::default()
    };

    accumulate_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 100);
    assert_eq!(dst.output_tokens, 50);
    assert_eq!(dst.reasoning_tokens, 25);
    assert_eq!(dst.cache_creation_tokens, 10);
    assert_eq!(dst.cache_read_tokens, 5);
    assert_eq!(dst.cached_tokens, 15);
    assert_eq!(dst.cost, 0.01);
    assert_eq!(dst.tool_calls, 3);
    assert_eq!(dst.terminal_commands, 2);
    assert_eq!(dst.file_searches, 1);
    assert_eq!(dst.files_read, 5);
    assert_eq!(dst.files_edited, 2);
    assert_eq!(dst.lines_added, 100);
    assert_eq!(dst.lines_deleted, 50);
    assert_eq!(dst.bytes_added, 5000);
}

#[test]
fn test_accumulate_stats_zero_values() {
    let mut dst = Stats::default();
    let src = Stats::default();

    accumulate_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 0);
    assert_eq!(dst.output_tokens, 0);
    assert_eq!(dst.cost, 0.0);
}

// ============================================================================
// SESSION AGGREGATION TESTS
// ============================================================================

#[test]
fn test_aggregate_sessions_single() {
    let date_utc = Utc.with_ymd_and_hms(2025, 11, 20, 2, 0, 0).unwrap();

    let msg = ConversationMessage {
        application: Application::GeminiCli,
        date: date_utc,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash".to_string(),
        local_hash: None,
        global_hash: "global_hash".to_string(),
        model: Some("model".to_string()),
        stats: Stats {
            input_tokens: 10,
            ..Stats::default()
        },
        role: MessageRole::Assistant,
        uuid: None,
        session_name: Some("Test Session".to_string()),
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg],
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "conv_hash");
    assert_eq!(sessions[0].session_name, Some("Test Session".to_string()));
    assert_eq!(sessions[0].stats.input_tokens, 10);
}

#[test]
fn test_aggregate_sessions_multiple_same_conversation() {
    let date1 = Utc.with_ymd_and_hms(2025, 11, 20, 2, 0, 0).unwrap();
    let date2 = Utc.with_ymd_and_hms(2025, 11, 20, 3, 0, 0).unwrap();

    let msg1 = ConversationMessage {
        application: Application::GeminiCli,
        date: date1,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash".to_string(),
        local_hash: None,
        global_hash: "global_hash1".to_string(),
        model: Some("model".to_string()),
        stats: Stats {
            input_tokens: 10,
            output_tokens: 5,
            ..Stats::default()
        },
        role: MessageRole::Assistant,
        uuid: None,
        session_name: Some("Test Session".to_string()),
    };

    let msg2 = ConversationMessage {
        date: date2,
        global_hash: "global_hash2".to_string(),
        stats: Stats {
            input_tokens: 20,
            output_tokens: 10,
            ..Stats::default()
        },
        ..msg1.clone()
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg1, msg2],
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].stats.input_tokens, 30);
    assert_eq!(sessions[0].stats.output_tokens, 15);
}

#[test]
fn test_aggregate_sessions_multiple_conversations() {
    let date1 = Utc.with_ymd_and_hms(2025, 11, 20, 2, 0, 0).unwrap();
    let date2 = Utc.with_ymd_and_hms(2025, 11, 20, 3, 0, 0).unwrap();

    let msg1 = ConversationMessage {
        application: Application::GeminiCli,
        date: date1,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash_1".to_string(),
        local_hash: None,
        global_hash: "global_hash1".to_string(),
        model: Some("model".to_string()),
        stats: Stats {
            input_tokens: 10,
            ..Stats::default()
        },
        role: MessageRole::Assistant,
        uuid: None,
        session_name: Some("Session 1".to_string()),
    };

    let msg2 = ConversationMessage {
        date: date2,
        conversation_hash: "conv_hash_2".to_string(),
        global_hash: "global_hash2".to_string(),
        session_name: Some("Session 2".to_string()),
        stats: Stats {
            input_tokens: 20,
            ..Stats::default()
        },
        ..msg1.clone()
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 2,
        messages: vec![msg1, msg2],
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].session_id, "conv_hash_1");
    assert_eq!(sessions[1].session_id, "conv_hash_2");
}

#[test]
fn test_aggregate_sessions_user_messages_ignored() {
    let date_utc = Utc.with_ymd_and_hms(2025, 11, 20, 2, 0, 0).unwrap();

    let msg_user = ConversationMessage {
        application: Application::GeminiCli,
        date: date_utc,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash".to_string(),
        local_hash: None,
        global_hash: "global_hash".to_string(),
        model: None, // User messages have no model
        stats: Stats {
            input_tokens: 100,
            ..Stats::default()
        },
        role: MessageRole::User,
        uuid: None,
        session_name: None,
    };

    let msg_assistant = ConversationMessage {
        model: Some("model".to_string()),
        role: MessageRole::Assistant,
        stats: Stats {
            input_tokens: 10,
            ..Stats::default()
        },
        ..msg_user.clone()
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg_user, msg_assistant],
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    // Only the assistant message should be counted
    assert_eq!(sessions[0].stats.input_tokens, 10);
}

#[test]
fn test_aggregate_sessions_sorting() {
    let date_early = Utc.with_ymd_and_hms(2025, 11, 20, 2, 0, 0).unwrap();
    let date_late = Utc.with_ymd_and_hms(2025, 11, 21, 2, 0, 0).unwrap();

    let msg_late = ConversationMessage {
        application: Application::GeminiCli,
        date: date_late,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash_late".to_string(),
        local_hash: None,
        global_hash: "global_hash_late".to_string(),
        model: Some("model".to_string()),
        stats: Stats::default(),
        role: MessageRole::Assistant,
        uuid: None,
        session_name: None,
    };

    let msg_early = ConversationMessage {
        date: date_early,
        conversation_hash: "conv_hash_early".to_string(),
        global_hash: "global_hash_early".to_string(),
        ..msg_late.clone()
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 2,
        messages: vec![msg_late, msg_early], // Add late first
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    // Should be sorted by timestamp, earliest first
    assert_eq!(sessions[0].session_id, "conv_hash_early");
    assert_eq!(sessions[1].session_id, "conv_hash_late");
}

#[test]
fn test_aggregate_sessions_multiple_models() {
    let date_utc = Utc.with_ymd_and_hms(2025, 11, 20, 2, 0, 0).unwrap();

    let msg1 = ConversationMessage {
        application: Application::GeminiCli,
        date: date_utc,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash".to_string(),
        local_hash: None,
        global_hash: "global_hash1".to_string(),
        model: Some("model-1".to_string()),
        stats: Stats::default(),
        role: MessageRole::Assistant,
        uuid: None,
        session_name: None,
    };

    let msg2 = ConversationMessage {
        model: Some("model-2".to_string()),
        global_hash: "global_hash2".to_string(),
        ..msg1.clone()
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg1, msg2],
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    assert_eq!(sessions[0].models.len(), 2);
    assert!(sessions[0].models.contains(&"model-1".to_string()));
    assert!(sessions[0].models.contains(&"model-2".to_string()));
}

// ============================================================================
// HAS_DATA TESTS
// ============================================================================

#[test]
fn test_has_data_empty() {
    let empty_stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 0,
        messages: vec![],
        analyzer_name: "Test".to_string(),
    };
    assert!(!has_data(&empty_stats));
}

#[test]
fn test_has_data_with_conversations() {
    let stats_with_conv = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![],
        analyzer_name: "Test".to_string(),
    };
    assert!(has_data(&stats_with_conv));
}

#[test]
fn test_has_data_with_cost() {
    let mut daily_stats = BTreeMap::new();
    let date_key = "2025-11-20".to_string();
    let day_stats = crate::types::DailyStats {
        date: "2025-11-20".to_string(),
        user_messages: 0,
        ai_messages: 0,
        conversations: 0,
        models: BTreeMap::new(),
        stats: Stats {
            cost: 0.01,
            ..Stats::default()
        },
    };
    daily_stats.insert(date_key, day_stats);

    let stats = AgenticCodingToolStats {
        daily_stats,
        num_conversations: 0,
        messages: vec![],
        analyzer_name: "Test".to_string(),
    };
    assert!(has_data(&stats));
}

#[test]
fn test_has_data_with_tokens() {
    let mut daily_stats = BTreeMap::new();
    let date_key = "2025-11-20".to_string();
    let day_stats = crate::types::DailyStats {
        date: "2025-11-20".to_string(),
        user_messages: 0,
        ai_messages: 0,
        conversations: 0,
        models: BTreeMap::new(),
        stats: Stats {
            input_tokens: 100,
            ..Stats::default()
        },
    };
    daily_stats.insert(date_key, day_stats);

    let stats = AgenticCodingToolStats {
        daily_stats,
        num_conversations: 0,
        messages: vec![],
        analyzer_name: "Test".to_string(),
    };
    assert!(has_data(&stats));
}

// ============================================================================
// AGGREGATE_SESSIONS_FOR_ALL_TOOLS TESTS
// ============================================================================

#[test]
fn test_aggregate_sessions_for_all_tools_empty() {
    let filtered_stats: Vec<&AgenticCodingToolStats> = vec![];
    let result = aggregate_sessions_for_all_tools(&filtered_stats);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_aggregate_sessions_for_all_tools_single() {
    let date_utc = Utc.with_ymd_and_hms(2025, 11, 20, 2, 0, 0).unwrap();
    let msg = ConversationMessage {
        application: Application::GeminiCli,
        date: date_utc,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash".to_string(),
        local_hash: None,
        global_hash: "global_hash".to_string(),
        model: Some("model".to_string()),
        stats: Stats {
            input_tokens: 10,
            ..Stats::default()
        },
        role: MessageRole::Assistant,
        uuid: None,
        session_name: Some("Test Session".to_string()),
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg],
        analyzer_name: "Test".to_string(),
    };

    let filtered_stats = vec![&stats];
    let result = aggregate_sessions_for_all_tools(&filtered_stats);

    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len(), 1);
    assert_eq!(result[0][0].session_id, "conv_hash");
}

#[test]
fn test_aggregate_sessions_for_all_tools_multiple() {
    let date1 = Utc.with_ymd_and_hms(2025, 11, 20, 2, 0, 0).unwrap();
    let date2 = Utc.with_ymd_and_hms(2025, 11, 20, 3, 0, 0).unwrap();

    let msg1 = ConversationMessage {
        application: Application::GeminiCli,
        date: date1,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash_1".to_string(),
        local_hash: None,
        global_hash: "global_hash_1".to_string(),
        model: Some("model".to_string()),
        stats: Stats {
            input_tokens: 10,
            ..Stats::default()
        },
        role: MessageRole::Assistant,
        uuid: None,
        session_name: None,
    };

    let msg2 = ConversationMessage {
        date: date2,
        conversation_hash: "conv_hash_2".to_string(),
        global_hash: "global_hash_2".to_string(),
        ..msg1.clone()
    };

    let stats1 = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg1],
        analyzer_name: "Claude Code".to_string(),
    };

    let stats2 = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg2],
        analyzer_name: "Copilot".to_string(),
    };

    let filtered_stats = vec![&stats1, &stats2];
    let result = aggregate_sessions_for_all_tools(&filtered_stats);

    assert_eq!(result.len(), 2);
    assert_eq!(result[0].len(), 1);
    assert_eq!(result[1].len(), 1);
}

// ============================================================================
// EDGE CASE TESTS
// ============================================================================

#[test]
fn test_date_matches_month_partial_prefix() {
    assert!(date_matches_buffer("2025-05-20", "may")); // May (3 char minimum)
    assert!(date_matches_buffer("2025-05-20", "MAY"));
}

#[test]
fn test_accumulate_stats_preserves_dst_initial_values() {
    let mut dst = Stats {
        input_tokens: 50,
        output_tokens: 25,
        cost: 0.005,
        ..Stats::default()
    };
    let src = Stats {
        input_tokens: 50,
        output_tokens: 25,
        cost: 0.005,
        ..Stats::default()
    };

    accumulate_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 100);
    assert_eq!(dst.output_tokens, 50);
    assert_eq!(dst.cost, 0.01);
}

#[test]
fn test_session_aggregate_captures_earliest_timestamp() {
    let date_late = Utc.with_ymd_and_hms(2025, 11, 21, 10, 0, 0).unwrap();
    let date_early = Utc.with_ymd_and_hms(2025, 11, 20, 12, 0, 0).unwrap();

    let msg_late = ConversationMessage {
        application: Application::GeminiCli,
        date: date_late,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash".to_string(),
        local_hash: None,
        global_hash: "global_hash_late".to_string(),
        model: Some("model".to_string()),
        stats: Stats::default(),
        role: MessageRole::Assistant,
        uuid: None,
        session_name: None,
    };

    let msg_early = ConversationMessage {
        date: date_early,
        global_hash: "global_hash_early".to_string(),
        ..msg_late.clone()
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg_late, msg_early],
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    assert_eq!(sessions[0].first_timestamp, date_early);
    // The day_key is derived from local time, just verify it starts with 2025-11
    assert!(sessions[0].day_key.starts_with("2025-11"));
}

#[test]
fn test_aggregate_sessions_deduplicates_models() {
    let date_utc = Utc.with_ymd_and_hms(2025, 11, 20, 2, 0, 0).unwrap();

    let msg1 = ConversationMessage {
        application: Application::GeminiCli,
        date: date_utc,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash".to_string(),
        local_hash: None,
        global_hash: "global_hash1".to_string(),
        model: Some("gpt-4".to_string()),
        stats: Stats::default(),
        role: MessageRole::Assistant,
        uuid: None,
        session_name: None,
    };

    let msg2 = ConversationMessage {
        model: Some("gpt-4".to_string()), // Same model
        global_hash: "global_hash2".to_string(),
        ..msg1.clone()
    };

    let msg3 = ConversationMessage {
        model: Some("gpt-3.5".to_string()), // Different model
        global_hash: "global_hash3".to_string(),
        ..msg1.clone()
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg1, msg2, msg3],
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    assert_eq!(sessions[0].models.len(), 2); // Only 2 unique models
}

#[test]
fn test_session_day_key_formatting() {
    // Use noon UTC to avoid timezone conversion causing day shift
    let date_utc = Utc.with_ymd_and_hms(2025, 1, 5, 12, 0, 0).unwrap();

    let msg = ConversationMessage {
        application: Application::GeminiCli,
        date: date_utc,
        project_hash: "hash".to_string(),
        conversation_hash: "conv_hash".to_string(),
        local_hash: None,
        global_hash: "global_hash".to_string(),
        model: Some("model".to_string()),
        stats: Stats::default(),
        role: MessageRole::Assistant,
        uuid: None,
        session_name: None,
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg],
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    // The day_key is in YYYY-MM-DD format and based on local time
    assert!(sessions[0].day_key.starts_with("2025-01"));
}

#[test]
fn test_large_accumulation() {
    let mut dst = Stats::default();
    for _ in 0..1000 {
        let src = Stats {
            input_tokens: 100,
            output_tokens: 50,
            cost: 0.01,
            ..Stats::default()
        };
        accumulate_stats(&mut dst, &src);
    }

    assert_eq!(dst.input_tokens, 100_000);
    assert_eq!(dst.output_tokens, 50_000);
    assert!((dst.cost - 10.0).abs() < 0.0001);
}

// ============================================================================
// COMPREHENSIVE DATA INTEGRITY TESTS
// ============================================================================

#[test]
fn test_accumulated_stats_correctness() {
    let mut dst = Stats::default();
    let src = Stats {
        input_tokens: 150,
        output_tokens: 75,
        reasoning_tokens: 50,
        cost: 0.025,
        tool_calls: 5,
        terminal_commands: 2,
        files_read: 10,
        lines_added: 250,
        ..Stats::default()
    };

    accumulate_stats(&mut dst, &src);
    accumulate_stats(&mut dst, &src);

    // Verify accumulated stats
    assert_eq!(dst.input_tokens, 300);
    assert_eq!(dst.output_tokens, 150);
    assert_eq!(dst.reasoning_tokens, 100);
    assert_eq!(dst.tool_calls, 10);
    assert_eq!(dst.terminal_commands, 4);
    assert_eq!(dst.files_read, 20);
    assert_eq!(dst.lines_added, 500);
    assert!((dst.cost - 0.05).abs() < 0.0001);
}

#[test]
fn test_session_aggregate_correctness() {
    let date_utc = Utc.with_ymd_and_hms(2025, 11, 20, 12, 0, 0).unwrap();

    let msg1 = ConversationMessage {
        application: Application::ClaudeCode,
        date: date_utc,
        project_hash: "proj123".to_string(),
        conversation_hash: "conv123".to_string(),
        local_hash: None,
        global_hash: "global123".to_string(),
        model: Some("claude-3-5-sonnet".to_string()),
        stats: Stats {
            input_tokens: 500,
            output_tokens: 250,
            cost: 0.05,
            ..Stats::default()
        },
        role: MessageRole::Assistant,
        uuid: None,
        session_name: Some("Bug Fix Session".to_string()),
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg1],
        analyzer_name: "Claude Code".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);

    // Verify session aggregate correctness
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, "conv123");
    assert_eq!(sessions[0].analyzer_name, "Claude Code");
    assert_eq!(
        sessions[0].session_name,
        Some("Bug Fix Session".to_string())
    );
    assert_eq!(sessions[0].models, vec!["claude-3-5-sonnet".to_string()]);
    assert_eq!(sessions[0].stats.input_tokens, 500);
    assert_eq!(sessions[0].stats.output_tokens, 250);
    assert!((sessions[0].stats.cost - 0.05).abs() < 0.0001);
}

#[test]
fn test_multi_session_aggregation_correctness() {
    let date1 = Utc.with_ymd_and_hms(2025, 11, 20, 12, 0, 0).unwrap();
    let date2 = Utc.with_ymd_and_hms(2025, 11, 21, 12, 0, 0).unwrap();

    let msg1 = ConversationMessage {
        application: Application::ClaudeCode,
        date: date1,
        project_hash: "proj".to_string(),
        conversation_hash: "conv1".to_string(),
        local_hash: None,
        global_hash: "global1".to_string(),
        model: Some("claude-3-5-sonnet".to_string()),
        stats: Stats {
            input_tokens: 100,
            output_tokens: 50,
            ..Stats::default()
        },
        role: MessageRole::Assistant,
        uuid: None,
        session_name: Some("Session 1".to_string()),
    };

    let msg2 = ConversationMessage {
        date: date2,
        conversation_hash: "conv2".to_string(),
        global_hash: "global2".to_string(),
        session_name: Some("Session 2".to_string()),
        stats: Stats {
            input_tokens: 200,
            output_tokens: 100,
            ..Stats::default()
        },
        ..msg1.clone()
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 2,
        messages: vec![msg1, msg2],
        analyzer_name: "Claude Code".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);

    // Verify multiple sessions
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].session_id, "conv1");
    assert_eq!(sessions[0].stats.input_tokens, 100);
    assert_eq!(sessions[0].stats.output_tokens, 50);
    assert_eq!(sessions[1].session_id, "conv2");
    assert_eq!(sessions[1].stats.input_tokens, 200);
    assert_eq!(sessions[1].stats.output_tokens, 100);
}

// ============================================================================
// STATE & NAVIGATION TESTS
// ============================================================================

#[test]
fn test_date_filter_with_january() {
    assert!(date_matches_buffer("2025-01-15", "1"));
    assert!(date_matches_buffer("2025-01-15", "jan"));
    assert!(date_matches_buffer("2025-01-15", "JAN"));
}

#[test]
fn test_date_filter_exact_day_and_month() {
    assert!(date_matches_buffer("2025-12-25", "12-25"));
    assert!(date_matches_buffer("2025-03-17", "3-17"));
    assert!(date_matches_buffer("2025-12-31", "12/31"));
}

#[test]
fn test_date_filter_year_month() {
    assert!(date_matches_buffer("2025-06-15", "2025-06"));
    assert!(date_matches_buffer("2024-12-01", "2024-12"));
}

#[test]
fn test_date_filter_exclusions() {
    assert!(!date_matches_buffer("2025-01-15", "2"));
    assert!(!date_matches_buffer("2025-01-15", "2025-02"));
    assert!(!date_matches_buffer("2025-12-31", "2024"));
}

// ============================================================================
// TUI LOOP INTEGRATION TESTS
// ============================================================================

#[test]
fn test_tui_quit_behavior() {
    let stats = make_tool_stats("with-data", true);
    let multi = MultiAnalyzerStats {
        analyzer_stats: vec![stats],
    };

    let events = vec![Event::Key(KeyEvent::new(
        KeyCode::Char('q'),
        KeyModifiers::empty(),
    ))];

    let (selected_tab, view_mode) = run_tui_with_events(multi, events, 10);
    assert_eq!(selected_tab, 0);
    assert_eq!(view_mode, StatsViewMode::Daily);
}

#[test]
fn test_tui_tab_switch_and_session_toggle() {
    let multi = make_multi_two_tools();

    let events = vec![
        Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
        Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty())),
    ];

    let (selected_tab, view_mode) = run_tui_with_events(multi, events, 50);
    assert_eq!(selected_tab, 1);
    assert_eq!(view_mode, StatsViewMode::Session);
}

#[test]
fn test_tui_date_jump_behavior() {
    let multi = make_multi_single_tool_two_days();

    let events = vec![
        Event::Key(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('5'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('-'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty())),
    ];

    let (_selected_tab, view_mode) = run_tui_with_events(multi, events, 80);
    assert_eq!(view_mode, StatsViewMode::Daily);
}

#[test]
fn test_tui_toggle_summary_panel() {
    let stats = make_tool_stats("with-data", true);
    let multi = MultiAnalyzerStats {
        analyzer_stats: vec![stats],
    };

    // Press 's' twice (toggle off, toggle on) then quit
    let events = vec![
        Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty())),
    ];

    // The test passes if run_tui_with_events completes without panic
    // The toggle state is internal, but we verify the key handling works
    let (selected_tab, view_mode) = run_tui_with_events(multi, events, 50);
    assert_eq!(selected_tab, 0);
    assert_eq!(view_mode, StatsViewMode::Daily);
}

#[test]
fn test_tui_drill_into_session_with_enter() {
    let multi = make_multi_single_tool_two_days();

    let events = vec![
        Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())),
        Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty())),
    ];

    let (_selected_tab, view_mode) = run_tui_with_events(multi, events, 80);
    assert_eq!(view_mode, StatsViewMode::Session);
}

#[test]
fn test_model_deduplication_in_session() {
    let date_utc = Utc.with_ymd_and_hms(2025, 11, 20, 12, 0, 0).unwrap();

    let models = vec!["gpt-4", "gpt-3.5", "gpt-4", "claude", "gpt-3.5"];
    let messages: Vec<_> = models
        .into_iter()
        .enumerate()
        .map(|(i, model)| ConversationMessage {
            application: Application::ClaudeCode,
            date: date_utc,
            project_hash: "hash".to_string(),
            conversation_hash: "conv".to_string(),
            local_hash: None,
            global_hash: format!("hash_{}", i),
            model: Some(model.to_string()),
            stats: Stats::default(),
            role: MessageRole::Assistant,
            uuid: None,
            session_name: None,
        })
        .collect();

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages,
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    // Should have 3 unique models: gpt-4, gpt-3.5, claude
    assert_eq!(sessions[0].models.len(), 3);
    assert!(sessions[0].models.contains(&"gpt-4".to_string()));
    assert!(sessions[0].models.contains(&"gpt-3.5".to_string()));
    assert!(sessions[0].models.contains(&"claude".to_string()));
}

#[test]
fn test_session_filtering_by_date_range() {
    let date1 = Utc.with_ymd_and_hms(2025, 11, 15, 12, 0, 0).unwrap();
    let date2 = Utc.with_ymd_and_hms(2025, 11, 20, 12, 0, 0).unwrap();
    let date3 = Utc.with_ymd_and_hms(2025, 12, 1, 12, 0, 0).unwrap();

    let messages = vec![
        ConversationMessage {
            application: Application::ClaudeCode,
            date: date1,
            project_hash: "hash".to_string(),
            conversation_hash: "conv1".to_string(),
            local_hash: None,
            global_hash: "global1".to_string(),
            model: Some("model".to_string()),
            stats: Stats::default(),
            role: MessageRole::Assistant,
            uuid: None,
            session_name: Some("Nov 15".to_string()),
        },
        ConversationMessage {
            date: date2,
            conversation_hash: "conv2".to_string(),
            global_hash: "global2".to_string(),
            session_name: Some("Nov 20".to_string()),
            ..ConversationMessage {
                application: Application::ClaudeCode,
                date: date1,
                project_hash: "hash".to_string(),
                conversation_hash: "conv1".to_string(),
                local_hash: None,
                global_hash: "global1".to_string(),
                model: Some("model".to_string()),
                stats: Stats::default(),
                role: MessageRole::Assistant,
                uuid: None,
                session_name: Some("Nov 15".to_string()),
            }
        },
        ConversationMessage {
            date: date3,
            conversation_hash: "conv3".to_string(),
            global_hash: "global3".to_string(),
            session_name: Some("Dec 01".to_string()),
            ..ConversationMessage {
                application: Application::ClaudeCode,
                date: date1,
                project_hash: "hash".to_string(),
                conversation_hash: "conv1".to_string(),
                local_hash: None,
                global_hash: "global1".to_string(),
                model: Some("model".to_string()),
                stats: Stats::default(),
                role: MessageRole::Assistant,
                uuid: None,
                session_name: Some("Nov 15".to_string()),
            }
        },
    ];

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 3,
        messages,
        analyzer_name: "Test".to_string(),
    };

    let sessions = aggregate_sessions_for_tool(&stats);
    assert_eq!(sessions.len(), 3);

    // November sessions should match
    assert!(date_matches_buffer(&sessions[0].day_key, "11"));
    assert!(date_matches_buffer(&sessions[1].day_key, "11"));
    // December session should not match
    assert!(!date_matches_buffer(&sessions[2].day_key, "11"));
}

#[test]
fn test_stats_accumulation_with_multiple_analyzers() {
    let mut dst = Stats::default();
    let src1 = Stats {
        input_tokens: 100,
        output_tokens: 50,
        cost: 0.01,
        tool_calls: 2,
        ..Stats::default()
    };
    let src2 = Stats {
        input_tokens: 200,
        output_tokens: 100,
        cost: 0.02,
        tool_calls: 4,
        ..Stats::default()
    };

    accumulate_stats(&mut dst, &src1);
    accumulate_stats(&mut dst, &src2);

    assert_eq!(dst.input_tokens, 300);
    assert_eq!(dst.output_tokens, 150);
    assert_eq!(dst.tool_calls, 6);
    assert!((dst.cost - 0.03).abs() < 0.0001);
}

#[test]
fn test_empty_analysis_state() {
    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 0,
        messages: vec![],
        analyzer_name: "Empty Analyzer".to_string(),
    };

    // has_data should return false for empty stats
    assert!(!has_data(&stats));

    // aggregate_sessions should return empty vec
    let sessions = aggregate_sessions_for_tool(&stats);
    assert_eq!(sessions.len(), 0);
}

#[test]
fn test_single_message_single_session_state() {
    let date_utc = Utc.with_ymd_and_hms(2025, 11, 20, 12, 0, 0).unwrap();
    let msg = ConversationMessage {
        application: Application::ClaudeCode,
        date: date_utc,
        project_hash: "hash".to_string(),
        conversation_hash: "conv".to_string(),
        local_hash: None,
        global_hash: "global".to_string(),
        model: Some("model".to_string()),
        stats: Stats {
            input_tokens: 50,
            output_tokens: 25,
            cost: 0.005,
            ..Stats::default()
        },
        role: MessageRole::Assistant,
        uuid: None,
        session_name: Some("Single Message Session".to_string()),
    };

    let stats = AgenticCodingToolStats {
        daily_stats: BTreeMap::new(),
        num_conversations: 1,
        messages: vec![msg],
        analyzer_name: "Test".to_string(),
    };

    // Verify state
    assert!(has_data(&stats));
    let sessions = aggregate_sessions_for_tool(&stats);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].stats.input_tokens, 50);
    assert_eq!(
        sessions[0].session_name,
        Some("Single Message Session".to_string())
    );
}
