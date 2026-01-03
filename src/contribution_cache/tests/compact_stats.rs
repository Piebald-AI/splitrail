//! Tests for PackedStatsDate bitfield operations

use super::super::single_message::PackedStatsDate;
use crate::types::{CompactDate, Stats};

#[test]
fn test_packed_stats_date_from_stats() {
    let stats = Stats {
        input_tokens: 1000,
        output_tokens: 500,
        reasoning_tokens: 100,
        cached_tokens: 200,
        cost: 0.05,
        tool_calls: 3,
        ..Default::default()
    };
    let date = CompactDate::from_parts(2025, 6, 15);

    let packed = PackedStatsDate::pack(&stats, date);

    assert_eq!(packed.input_tokens(), 1000);
    assert_eq!(packed.output_tokens(), 500);
    assert_eq!(packed.reasoning_tokens(), 100);
    assert_eq!(packed.cached_tokens(), 200);
    assert_eq!(packed.cost_cents(), 5); // 0.05 * 100 = 5 cents
    assert_eq!(packed.tool_calls(), 3);

    let unpacked_date = packed.unpack_date();
    assert_eq!(unpacked_date.year(), 2025);
    assert_eq!(unpacked_date.month(), 6);
    assert_eq!(unpacked_date.day(), 15);
}

#[test]
fn test_packed_stats_date_to_tui_stats() {
    let stats = Stats {
        input_tokens: 1000,
        output_tokens: 500,
        reasoning_tokens: 100,
        cached_tokens: 200,
        cost: 0.50,
        tool_calls: 5,
        ..Default::default()
    };
    let date = CompactDate::from_parts(2025, 1, 1);

    let packed = PackedStatsDate::pack(&stats, date);
    let tui = packed.to_tui_stats();

    assert_eq!(tui.input_tokens, 1000);
    assert_eq!(tui.output_tokens, 500);
    assert_eq!(tui.reasoning_tokens, 100);
    assert_eq!(tui.cached_tokens, 200);
    assert_eq!(tui.cost_cents, 50);
    assert_eq!(tui.tool_calls, 5);
}

#[test]
fn test_packed_stats_date_max_values() {
    // Test maximum values within bit limits (from diagnostic reference)
    let stats = Stats {
        input_tokens: 134_217_727, // 27-bit max
        output_tokens: 67_108_863, // 26-bit max
        reasoning_tokens: 67_108_863,
        cached_tokens: 134_217_727,
        cost: 655.35,       // u16 max cents
        tool_calls: 16_383, // 14-bit max
        ..Default::default()
    };
    let date = CompactDate::from_parts(2083, 12, 31); // Max year (2020 + 63)

    let packed = PackedStatsDate::pack(&stats, date);

    assert_eq!(packed.input_tokens(), 134_217_727);
    assert_eq!(packed.output_tokens(), 67_108_863);
    assert_eq!(packed.reasoning_tokens(), 67_108_863);
    assert_eq!(packed.cached_tokens(), 134_217_727);
    assert_eq!(packed.cost_cents(), 65535);
    assert_eq!(packed.tool_calls(), 16383);

    let unpacked_date = packed.unpack_date();
    assert_eq!(unpacked_date.year(), 2083);
    assert_eq!(unpacked_date.month(), 12);
    assert_eq!(unpacked_date.day(), 31);
}

#[test]
fn test_packed_stats_date_saturation() {
    // Test values beyond bit limits get saturated
    let stats = Stats {
        input_tokens: 200_000_000,  // Exceeds 27-bit max
        output_tokens: 100_000_000, // Exceeds 26-bit max
        reasoning_tokens: 100_000_000,
        cached_tokens: 200_000_000,
        cost: 1000.00,      // Exceeds u16 max cents
        tool_calls: 50_000, // Exceeds 14-bit max
        ..Default::default()
    };
    let date = CompactDate::from_parts(2100, 1, 1); // Exceeds max year

    let packed = PackedStatsDate::pack(&stats, date);

    // Values should be saturated to max
    assert_eq!(packed.input_tokens(), 0x7FF_FFFF); // 27-bit max
    assert_eq!(packed.output_tokens(), 0x3FF_FFFF); // 26-bit max
    assert_eq!(packed.cost_cents(), 65535); // u16 max
    assert_eq!(packed.tool_calls(), 16383); // 14-bit max

    let unpacked_date = packed.unpack_date();
    assert_eq!(unpacked_date.year(), 2083); // Saturated to 2020 + 63
}

#[test]
fn test_packed_stats_date_observed_values() {
    // Test with actual observed maximum values from diagnostic
    let stats = Stats {
        input_tokens: 170_749,
        output_tokens: 31_999,
        reasoning_tokens: 7_005,
        cached_tokens: 186_677,
        cost: 3.56,
        tool_calls: 73,
        ..Default::default()
    };
    let date = CompactDate::from_parts(2025, 6, 15);

    let packed = PackedStatsDate::pack(&stats, date);

    assert_eq!(packed.input_tokens(), 170_749);
    assert_eq!(packed.output_tokens(), 31_999);
    assert_eq!(packed.reasoning_tokens(), 7_005);
    assert_eq!(packed.cached_tokens(), 186_677);
    assert_eq!(packed.cost_cents(), 356);
    assert_eq!(packed.tool_calls(), 73);

    let unpacked_date = packed.unpack_date();
    assert_eq!(unpacked_date.year(), 2025);
    assert_eq!(unpacked_date.month(), 6);
    assert_eq!(unpacked_date.day(), 15);
}

#[test]
fn test_packed_stats_date_zero_values() {
    let stats = Stats::default();
    let date = CompactDate::from_parts(2020, 1, 1); // Minimum year

    let packed = PackedStatsDate::pack(&stats, date);

    assert_eq!(packed.input_tokens(), 0);
    assert_eq!(packed.output_tokens(), 0);
    assert_eq!(packed.reasoning_tokens(), 0);
    assert_eq!(packed.cached_tokens(), 0);
    assert_eq!(packed.cost_cents(), 0);
    assert_eq!(packed.tool_calls(), 0);

    let unpacked_date = packed.unpack_date();
    assert_eq!(unpacked_date.year(), 2020);
    assert_eq!(unpacked_date.month(), 1);
    assert_eq!(unpacked_date.day(), 1);
}
