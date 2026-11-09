//! Deterministic trace and span identifiers.
//!
//! Random ids would make a re-export of the same message look like a new trace,
//! which defeats the point of exporting history that gets rewritten on resume
//! or compaction. Deriving ids from the message's `global_hash` means the same
//! message always lands on the same span, so backends that upsert by span id
//! stay correct and a retried run is idempotent.

use xxhash_rust::xxh3::{xxh3_64, xxh3_128};

/// 16-byte trace id, lowercase hex (32 chars). Never all zero.
pub fn trace_id(seed: &str) -> String {
    let hash = xxh3_128(seed.as_bytes());
    format!("{:032x}", hash.max(1))
}

/// 8-byte span id, lowercase hex (16 chars). Never all zero.
pub fn span_id(seed: &str) -> String {
    let hash = xxh3_64(seed.as_bytes());
    format!("{:016x}", hash.max(1))
}

/// Trace id for a turn: stable for the messages that make up the turn.
pub fn trace_id_for(analyzer: &str, conversation: &str, first_message: &str) -> String {
    trace_id(&format!(
        "{analyzer}\u{1f}{conversation}\u{1f}{first_message}"
    ))
}

/// Span id for a turn's `invoke_agent` span.
pub fn turn_span_id(analyzer: &str, conversation: &str, first_message: &str) -> String {
    span_id(&format!(
        "{analyzer}\u{1f}{conversation}\u{1f}turn\u{1f}{first_message}"
    ))
}

/// Span id for an `execute_tool` span. Stable per call, so re-exports and
/// retries land on the same span.
pub fn tool_span_id(analyzer: &str, conversation: &str, call_key: &str) -> String {
    span_id(&format!(
        "{analyzer}\u{1f}{conversation}\u{1f}tool\u{1f}{call_key}"
    ))
}

/// Span id for a `chat` span (one assistant message / billed request).
pub fn chat_span_id(analyzer: &str, conversation: &str, global_hash: &str) -> String {
    span_id(&format!(
        "{analyzer}\u{1f}{conversation}\u{1f}chat\u{1f}{global_hash}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_hex_of_the_right_width() {
        let trace = trace_id("a");
        let span = span_id("a");
        assert_eq!(trace.len(), 32);
        assert_eq!(span.len(), 16);
        assert!(trace.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(span.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ids_are_deterministic() {
        assert_eq!(trace_id("seed"), trace_id("seed"));
        assert_eq!(
            chat_span_id("Codex CLI", "conv", "hash"),
            chat_span_id("Codex CLI", "conv", "hash")
        );
    }

    #[test]
    fn tool_span_ids_are_stable_and_distinct() {
        assert_eq!(
            tool_span_id("Claude Code", "conv", "toolu_1"),
            tool_span_id("Claude Code", "conv", "toolu_1")
        );
        assert_ne!(
            tool_span_id("Claude Code", "conv", "toolu_1"),
            tool_span_id("Claude Code", "conv", "toolu_2")
        );
    }

    #[test]
    fn ids_differ_for_different_inputs() {
        assert_ne!(
            chat_span_id("Codex CLI", "conv", "hash-a"),
            chat_span_id("Codex CLI", "conv", "hash-b")
        );
        assert_ne!(
            turn_span_id("Codex CLI", "conv", "hash"),
            chat_span_id("Codex CLI", "conv", "hash")
        );
    }

    #[test]
    fn ids_are_never_zero() {
        // An all-zero trace or span id is invalid OTLP.
        for seed in ["", "0", "x"] {
            assert_ne!(trace_id(seed), "0".repeat(32));
            assert_ne!(span_id(seed), "0".repeat(16));
        }
    }
}
