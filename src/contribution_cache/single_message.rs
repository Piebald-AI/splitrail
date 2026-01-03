//! Single-message contribution type for 1-file-1-message analyzers.
//!
//! Optimized to 32 bytes for cache alignment using bitfield packing.

use c2rust_bitfields::BitfieldStruct;

use super::SessionHash;
use crate::cache::ModelKey;
use crate::types::{CompactDate, ConversationMessage, TuiStats, intern_model};

// ============================================================================
// PackedStatsDate - Bitfield-packed stats and date (22 bytes)
// ============================================================================

// Diagnostic reference (95,532 messages analyzed):
//
// | Field             | Observed Max | Rec. Bits | Rec. Bits Max     |
// |-------------------|--------------|-----------|-------------------|
// | input_tokens      | 170,749      | 27        | 134,217,727       |
// | output_tokens     | 31,999       | 26        | 67,108,863        |
// | reasoning_tokens  | 7,005        | 26        | 67,108,863        |
// | cached_tokens     | 186,677      | 27        | 134,217,727       |
// | cost_cents        | 356          | 16        | 65,535 ($655.35)  |
// | tool_calls        | 73           | 14        | 16,383            |
// | year_offset       | 2025-2026    | 6         | 63 (2020-2083)    |
// | month             | 1-12         | 4         | 15                |
// | day               | 1-31         | 5         | 31                |
// | duration_ms       | —            | 25        | 33,554,431 (~9.3h)|
//
// Total: 176 bits = 22 bytes

/// Packed stats and date in 176 bits (22 bytes).
///
/// Layout:
/// - input_tokens:     bits 0-26   (27 bits, max 134,217,727)
/// - output_tokens:    bits 27-52  (26 bits, max 67,108,863)
/// - reasoning_tokens: bits 53-78  (26 bits, max 67,108,863)
/// - cached_tokens:    bits 79-105 (27 bits, max 134,217,727)
/// - cost_cents:       bits 106-121 (16 bits, max 65,535 = $655.35)
/// - tool_calls:       bits 122-135 (14 bits, max 16,383)
/// - year_offset:      bits 136-141 (6 bits, years 2020-2083)
/// - month:            bits 142-145 (4 bits, 1-12)
/// - day:              bits 146-150 (5 bits, 1-31)
/// - duration_ms:      bits 151-175 (25 bits, max ~9.3 hours)
#[repr(C, align(1))]
#[derive(BitfieldStruct, Clone, Copy, Default)]
pub struct PackedStatsDate {
    #[bitfield(name = "input_tokens", ty = "u32", bits = "0..=26")]
    #[bitfield(name = "output_tokens", ty = "u32", bits = "27..=52")]
    #[bitfield(name = "reasoning_tokens", ty = "u32", bits = "53..=78")]
    #[bitfield(name = "cached_tokens", ty = "u32", bits = "79..=105")]
    #[bitfield(name = "cost_cents", ty = "u16", bits = "106..=121")]
    #[bitfield(name = "tool_calls", ty = "u16", bits = "122..=135")]
    #[bitfield(name = "year_offset", ty = "u8", bits = "136..=141")]
    #[bitfield(name = "month", ty = "u8", bits = "142..=145")]
    #[bitfield(name = "day", ty = "u8", bits = "146..=150")]
    #[bitfield(name = "duration_ms", ty = "u32", bits = "151..=175")]
    data: [u8; 22],
}

impl std::fmt::Debug for PackedStatsDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PackedStatsDate")
            .field("input_tokens", &self.input_tokens())
            .field("output_tokens", &self.output_tokens())
            .field("reasoning_tokens", &self.reasoning_tokens())
            .field("cached_tokens", &self.cached_tokens())
            .field("cost_cents", &self.cost_cents())
            .field("tool_calls", &self.tool_calls())
            .field("year_offset", &self.year_offset())
            .field("month", &self.month())
            .field("day", &self.day())
            .field("duration_ms", &self.duration_ms())
            .finish()
    }
}

/// Base year for year_offset encoding (6 bits covers 2020-2083).
const BASE_YEAR: u16 = 2020;

impl PackedStatsDate {
    /// Pack stats and date into the bitfield.
    #[inline]
    pub fn pack(stats: &crate::types::Stats, date: CompactDate) -> Self {
        let mut packed = Self::default();

        // Pack stats (with saturation for safety)
        packed.set_input_tokens(stats.input_tokens.min(0x7FF_FFFF) as u32);
        packed.set_output_tokens(stats.output_tokens.min(0x3FF_FFFF) as u32);
        packed.set_reasoning_tokens(stats.reasoning_tokens.min(0x3FF_FFFF) as u32);
        packed.set_cached_tokens(stats.cached_tokens.min(0x7FF_FFFF) as u32);
        packed.set_cost_cents((stats.cost * 100.0).round().min(u16::MAX as f64) as u16);
        packed.set_tool_calls(stats.tool_calls.min(0x3FFF) as u16);

        // Pack date
        let year_offset = date.year().saturating_sub(BASE_YEAR).min(63) as u8;
        packed.set_year_offset(year_offset);
        packed.set_month(date.month());
        packed.set_day(date.day());

        // duration_ms reserved for future use
        packed.set_duration_ms(0);

        packed
    }

    /// Extract date from packed representation.
    #[inline]
    pub fn unpack_date(&self) -> CompactDate {
        CompactDate::from_parts(
            BASE_YEAR + self.year_offset() as u16,
            self.month(),
            self.day(),
        )
    }

    /// Convert packed stats to TuiStats for display.
    #[inline]
    pub fn to_tui_stats(self) -> TuiStats {
        TuiStats {
            input_tokens: self.input_tokens(),
            output_tokens: self.output_tokens(),
            reasoning_tokens: self.reasoning_tokens(),
            cached_tokens: self.cached_tokens(),
            cost_cents: self.cost_cents() as u32,
            tool_calls: self.tool_calls() as u32,
        }
    }
}

// ============================================================================
// SingleMessageContribution - For 1 file = 1 message analyzers (32 bytes)
// ============================================================================

/// Lightweight contribution for single-message-per-file analyzers.
/// Uses 32 bytes (cache-aligned) instead of previous 40 bytes.
/// Designed for analyzers like OpenCode where each file contains exactly one message.
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct SingleMessageContribution {
    /// Hash of conversation_hash for session lookup (avoids String allocation)
    pub session_hash: SessionHash, // 8 bytes (offset 0)
    /// Model used (interned key), None if no model specified
    pub model: Option<ModelKey>, // 2 bytes (offset 8, niche-optimized)
    /// Packed stats and date
    pub packed: PackedStatsDate, // 22 bytes (offset 10)
} // Total: 32 bytes

// Compile-time size assertion
const _: () = assert!(std::mem::size_of::<SingleMessageContribution>() == 32);

impl SingleMessageContribution {
    /// Create from a single message.
    #[inline]
    pub fn from_message(msg: &ConversationMessage) -> Self {
        Self {
            session_hash: SessionHash::from_str(&msg.conversation_hash),
            model: msg.model.as_ref().map(|m| intern_model(m)),
            packed: PackedStatsDate::pack(&msg.stats, CompactDate::from_local(&msg.date)),
        }
    }

    /// Get the date from the packed representation.
    #[inline]
    pub fn date(&self) -> CompactDate {
        self.packed.unpack_date()
    }

    /// Convert packed stats to TuiStats for display.
    #[inline]
    pub fn to_tui_stats(self) -> TuiStats {
        self.packed.to_tui_stats()
    }

    /// Hash a session_id string for comparison with stored session_hash.
    #[inline]
    pub fn hash_session_id(session_id: &str) -> SessionHash {
        SessionHash::from_str(session_id)
    }
}

#[cfg(test)]
mod size_tests {
    use super::*;
    use std::mem::{align_of, size_of};

    #[test]
    fn struct_sizes_optimized() {
        println!("\n=== Struct Size Analysis ===");
        println!(
            "PackedStatsDate: {} bytes, align {}",
            size_of::<PackedStatsDate>(),
            align_of::<PackedStatsDate>()
        );
        println!(
            "Option<ModelKey>: {} bytes, align {}",
            size_of::<Option<ModelKey>>(),
            align_of::<Option<ModelKey>>()
        );
        println!(
            "SessionHash: {} bytes, align {}",
            size_of::<SessionHash>(),
            align_of::<SessionHash>()
        );
        println!(
            "SingleMessageContribution: {} bytes, align {}",
            size_of::<SingleMessageContribution>(),
            align_of::<SingleMessageContribution>()
        );
        println!("=== End Analysis ===\n");

        // Verify sizes
        assert_eq!(size_of::<PackedStatsDate>(), 22);
        assert_eq!(size_of::<SingleMessageContribution>(), 32);
    }

    #[test]
    fn bitfield_roundtrip() {
        use crate::types::Stats;

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
    fn bitfield_max_values() {
        use crate::types::Stats;

        // Test maximum values within bit limits
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
        assert_eq!(packed.cost_cents(), 65535);
        assert_eq!(packed.tool_calls(), 16383);

        let unpacked_date = packed.unpack_date();
        assert_eq!(unpacked_date.year(), 2083);
        assert_eq!(unpacked_date.month(), 12);
        assert_eq!(unpacked_date.day(), 31);
    }
}
