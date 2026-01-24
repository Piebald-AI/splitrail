use crate::tui::logic::{accumulate_tui_stats, date_matches_buffer};
use crate::tui::{
    create_upload_progress_callback, show_upload_error, show_upload_success, update_day_filters,
    update_table_states, update_window_offsets,
};
use crate::types::{AgenticCodingToolStats, CompactDate, MultiAnalyzerStats, Stats, TuiStats};
use ratatui::widgets::TableState;
use std::collections::BTreeMap;

// ============================================================================
// TABLE STATE MANAGEMENT TESTS (tui.rs helpers)
// ============================================================================

fn make_tool_stats(name: &str, has_data: bool) -> AgenticCodingToolStats {
    let mut daily_stats = BTreeMap::new();
    if has_data {
        daily_stats.insert(
            "2025-01-01".to_string(),
            crate::types::DailyStats {
                date: CompactDate::from_str("2025-01-01").unwrap(),
                user_messages: 0,
                ai_messages: 1,
                conversations: 1,
                models: BTreeMap::new(),
                stats: TuiStats {
                    input_tokens: 10,
                    ..TuiStats::default()
                },
                model_stats: BTreeMap::new(),
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

#[test]
fn test_update_table_states_filters_and_preserves_selection() {
    let stats_with_data = make_tool_stats("with-data", true);
    let stats_without_data = make_tool_stats("without-data", false);

    let multi = MultiAnalyzerStats {
        analyzer_stats: vec![stats_with_data, stats_without_data],
    };
    let multi_view = multi.into_view();

    let mut table_states: Vec<TableState> = Vec::new();
    let mut selected_tab = 0usize;

    update_table_states(&mut table_states, &multi_view, &mut selected_tab);

    // Only analyzers with data should be represented.
    assert_eq!(table_states.len(), 1);
    assert_eq!(selected_tab, 0);
    assert_eq!(table_states[0].selected(), Some(0));

    // If selected_tab is out of range, it should be clamped.
    let mut table_states = vec![TableState::default(); 1];
    let mut selected_tab = 10usize;
    let multi2 = MultiAnalyzerStats {
        analyzer_stats: vec![
            make_tool_stats("with-data", true),
            make_tool_stats("without-data", false),
        ],
    };
    let multi_view2 = multi2.into_view();
    update_table_states(&mut table_states, &multi_view2, &mut selected_tab);
    assert_eq!(selected_tab, 0);
}

#[test]
fn test_update_window_offsets_and_day_filters_resize() {
    let mut offsets = vec![5usize];
    let day = CompactDate::from_str("2025-01-01").unwrap();
    let mut filters: Vec<Option<CompactDate>> = vec![Some(day)];

    let count_two = 2usize;
    update_window_offsets(&mut offsets, &count_two);
    update_day_filters(&mut filters, &count_two);

    assert_eq!(offsets, vec![5, 0]);
    assert_eq!(filters, vec![Some(day), None]);

    let count_one = 1usize;
    update_window_offsets(&mut offsets, &count_one);
    update_day_filters(&mut filters, &count_one);

    assert_eq!(offsets, vec![5]);
    assert_eq!(filters, vec![Some(day)]);
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
fn test_accumulate_tui_stats_basic() {
    let mut dst = TuiStats::default();
    let src = Stats {
        input_tokens: 100,
        output_tokens: 50,
        cost: 0.01,
        ..Stats::default()
    };

    accumulate_tui_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 100);
    assert_eq!(dst.output_tokens, 50);
    assert_eq!(dst.cost(), 0.01);
}

#[test]
fn test_accumulate_tui_stats_multiple_times() {
    let mut dst = TuiStats::default();
    let src = Stats {
        input_tokens: 100,
        output_tokens: 50,
        cost: 0.01,
        ..Stats::default()
    };

    accumulate_tui_stats(&mut dst, &src);
    accumulate_tui_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 200);
    assert_eq!(dst.output_tokens, 100);
    assert_eq!(dst.cost(), 0.02);
}

#[test]
fn test_accumulate_tui_stats_comprehensive() {
    let mut dst = TuiStats::default();
    let src = Stats {
        input_tokens: 100,
        output_tokens: 50,
        reasoning_tokens: 25,
        cached_tokens: 15,
        cost: 0.01,
        tool_calls: 3,
        // File operation fields exist in Stats but are not accumulated into TuiStats
        ..Stats::default()
    };

    accumulate_tui_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 100);
    assert_eq!(dst.output_tokens, 50);
    assert_eq!(dst.reasoning_tokens, 25);
    assert_eq!(dst.cached_tokens, 15);
    assert_eq!(dst.cost(), 0.01);
    assert_eq!(dst.tool_calls, 3);
}

#[test]
fn test_accumulate_tui_stats_zero_values() {
    let mut dst = TuiStats::default();
    let src = Stats::default();

    accumulate_tui_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 0);
    assert_eq!(dst.output_tokens, 0);
    assert_eq!(dst.cost(), 0.0);
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
fn test_accumulate_tui_stats_preserves_dst_initial_values() {
    let mut dst = TuiStats {
        input_tokens: 50,
        output_tokens: 25,
        cost_cents: 1, // 0.01 dollars
        ..TuiStats::default()
    };
    let src = Stats {
        input_tokens: 50,
        output_tokens: 25,
        cost: 0.01,
        ..Stats::default()
    };

    accumulate_tui_stats(&mut dst, &src);
    assert_eq!(dst.input_tokens, 100);
    assert_eq!(dst.output_tokens, 50);
    assert_eq!(dst.cost(), 0.02);
}

#[test]
fn test_large_tui_stats_accumulation() {
    let mut dst = TuiStats::default();
    for _ in 0..1000 {
        let src = Stats {
            input_tokens: 100,
            output_tokens: 50,
            cost: 0.01,
            ..Stats::default()
        };
        accumulate_tui_stats(&mut dst, &src);
    }

    assert_eq!(dst.input_tokens, 100_000);
    assert_eq!(dst.output_tokens, 50_000);
    assert!((dst.cost() - 10.0).abs() < 0.01);
}

// ============================================================================
// COMPREHENSIVE DATA INTEGRITY TESTS
// ============================================================================

#[test]
fn test_accumulated_tui_stats_correctness() {
    let mut dst = TuiStats::default();
    let src = Stats {
        input_tokens: 150,
        output_tokens: 75,
        reasoning_tokens: 50,
        cost: 0.025,
        tool_calls: 5,
        // File operation fields not tracked in TuiStats
        ..Stats::default()
    };

    accumulate_tui_stats(&mut dst, &src);
    accumulate_tui_stats(&mut dst, &src);

    // Verify accumulated TUI stats (only the 6 display fields)
    assert_eq!(dst.input_tokens, 300);
    assert_eq!(dst.output_tokens, 150);
    assert_eq!(dst.reasoning_tokens, 100);
    assert_eq!(dst.tool_calls, 10);
    assert!((dst.cost() - 0.05).abs() < 0.01);
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

#[test]
fn test_tui_stats_accumulation_with_multiple_analyzers() {
    let mut dst = TuiStats::default();
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

    accumulate_tui_stats(&mut dst, &src1);
    accumulate_tui_stats(&mut dst, &src2);

    assert_eq!(dst.input_tokens, 300);
    assert_eq!(dst.output_tokens, 150);
    assert_eq!(dst.tool_calls, 6);
    assert!((dst.cost() - 0.03).abs() < 0.01);
}
