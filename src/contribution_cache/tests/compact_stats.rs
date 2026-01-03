//! Tests for CompactMessageStats operations

use super::super::CompactMessageStats;
use crate::types::Stats;

#[test]
fn test_compact_message_stats_from_stats() {
    let stats = Stats {
        input_tokens: 1000,
        output_tokens: 500,
        reasoning_tokens: 100,
        cached_tokens: 200,
        cost: 0.05,
        tool_calls: 3,
        ..Default::default()
    };

    let compact = CompactMessageStats::from_stats(&stats);

    assert_eq!(compact.input_tokens, 1000);
    assert_eq!(compact.output_tokens, 500);
    assert_eq!(compact.reasoning_tokens, 100);
    assert_eq!(compact.cached_tokens, 200);
    assert_eq!(compact.cost_cents, 5); // 0.05 * 100 = 5 cents
    assert_eq!(compact.tool_calls, 3);
}

#[test]
fn test_compact_message_stats_add_assign() {
    let mut a = CompactMessageStats {
        input_tokens: 100,
        output_tokens: 50,
        reasoning_tokens: 10,
        cached_tokens: 20,
        cost_cents: 5,
        tool_calls: 2,
    };
    let b = CompactMessageStats {
        input_tokens: 200,
        output_tokens: 100,
        reasoning_tokens: 20,
        cached_tokens: 40,
        cost_cents: 10,
        tool_calls: 3,
    };

    a += b;

    assert_eq!(a.input_tokens, 300);
    assert_eq!(a.output_tokens, 150);
    assert_eq!(a.reasoning_tokens, 30);
    assert_eq!(a.cached_tokens, 60);
    assert_eq!(a.cost_cents, 15);
    assert_eq!(a.tool_calls, 5);
}

#[test]
fn test_compact_message_stats_sub_assign() {
    let mut a = CompactMessageStats {
        input_tokens: 300,
        output_tokens: 150,
        reasoning_tokens: 30,
        cached_tokens: 60,
        cost_cents: 15,
        tool_calls: 5,
    };
    let b = CompactMessageStats {
        input_tokens: 100,
        output_tokens: 50,
        reasoning_tokens: 10,
        cached_tokens: 20,
        cost_cents: 5,
        tool_calls: 2,
    };

    a -= b;

    assert_eq!(a.input_tokens, 200);
    assert_eq!(a.output_tokens, 100);
    assert_eq!(a.reasoning_tokens, 20);
    assert_eq!(a.cached_tokens, 40);
    assert_eq!(a.cost_cents, 10);
    assert_eq!(a.tool_calls, 3);
}

#[test]
fn test_compact_message_stats_saturating_sub() {
    let mut a = CompactMessageStats {
        input_tokens: 50,
        output_tokens: 25,
        reasoning_tokens: 5,
        cached_tokens: 10,
        cost_cents: 2,
        tool_calls: 1,
    };
    let b = CompactMessageStats {
        input_tokens: 100,
        output_tokens: 50,
        reasoning_tokens: 10,
        cached_tokens: 20,
        cost_cents: 5,
        tool_calls: 3,
    };

    a -= b;

    // Should saturate to 0, not underflow
    assert_eq!(a.input_tokens, 0);
    assert_eq!(a.output_tokens, 0);
    assert_eq!(a.reasoning_tokens, 0);
    assert_eq!(a.cached_tokens, 0);
    assert_eq!(a.cost_cents, 0);
    assert_eq!(a.tool_calls, 0);
}

#[test]
fn test_compact_message_stats_to_tui_stats() {
    let compact = CompactMessageStats {
        input_tokens: 1000,
        output_tokens: 500,
        reasoning_tokens: 100,
        cached_tokens: 200,
        cost_cents: 50,
        tool_calls: 5,
    };

    let tui = compact.to_tui_stats();

    assert_eq!(tui.input_tokens, 1000);
    assert_eq!(tui.output_tokens, 500);
    assert_eq!(tui.reasoning_tokens, 100);
    assert_eq!(tui.cached_tokens, 200);
    assert_eq!(tui.cost_cents, 50);
    assert_eq!(tui.tool_calls, 5);
}
