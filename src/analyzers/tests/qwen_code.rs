use crate::analyzer::Analyzer;
use crate::analyzers::qwen_code::{QwenCodeAnalyzer, parse_jsonl_session_file};
use crate::types::MessageRole;
use std::collections::HashSet;
use std::path::PathBuf;

#[test]
fn test_qwen_code_analyzer_creation() {
    let analyzer = QwenCodeAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Qwen Code");
}

#[test]
fn test_qwen_code_is_available() {
    let analyzer = QwenCodeAnalyzer::new();
    // is_available depends on whether Qwen Code data exists.
    // Just verify it doesn't panic.
    let _ = analyzer.is_available();
}

#[test]
fn test_qwen_code_discover_data_sources_no_panic() {
    let analyzer = QwenCodeAnalyzer::new();
    // Should return Ok even if no data exists.
    let result = analyzer.discover_data_sources();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_qwen_code_get_stats_empty_sources() {
    let analyzer = QwenCodeAnalyzer::new();
    let result = analyzer.get_stats_with_sources(vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().messages.is_empty());
}

#[test]
fn test_qwen_code_glob_patterns_use_projects_dir() {
    let analyzer = QwenCodeAnalyzer::new();
    let patterns = analyzer.get_data_glob_patterns();
    assert!(!patterns.is_empty(), "Should have glob patterns defined");
    let joined = patterns.join(" ");
    assert!(
        joined.contains(".qwen/projects/"),
        "Patterns should target the new ~/.qwen/projects location, got: {joined}"
    );
    assert!(
        joined.contains("chats/*.jsonl"),
        "Patterns should include JSONL chat logs, got: {joined}"
    );
}

#[test]
fn test_parse_sample_qwen_code_session() {
    let sample_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("analyzers")
        .join("tests")
        .join("source_data")
        .join("qwen_code.jsonl");

    if !sample_path.exists() {
        // Skip test if sample file doesn't exist.
        return;
    }

    let messages =
        parse_jsonl_session_file(&sample_path).expect("sample Qwen Code session should parse");

    // The fixture contains: 1 user turn, 2 assistant turns (one tool call,
    // one final reply). tool_result and system records are dropped.
    assert_eq!(
        messages.len(),
        3,
        "Should parse 1 user + 2 assistant messages"
    );

    // Hash uniqueness.
    let mut hashes = HashSet::new();
    for msg in &messages {
        assert!(
            hashes.insert(msg.global_hash.clone()),
            "All message global hashes should be unique"
        );
    }

    // Message 0: user, no tokens.
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[0].stats.input_tokens, 0);
    assert_eq!(messages[0].stats.output_tokens, 0);

    // Message 1: assistant tool-call turn. usageMetadata:
    //   promptTokenCount=500, candidatesTokenCount=20, cached=0, thoughts=0.
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].stats.input_tokens, 500);
    assert_eq!(messages[1].stats.output_tokens, 20);
    assert_eq!(messages[1].stats.reasoning_tokens, 0);
    assert_eq!(messages[1].stats.cached_tokens, 0);
    assert_eq!(
        messages[1].stats.tool_calls, 1,
        "tool-call turn should record one tool call"
    );

    // Message 2: assistant final turn. usageMetadata:
    //   promptTokenCount=600, candidatesTokenCount=5, cached=50, thoughts=0.
    // input_tokens should exclude the cached portion (600 - 50 = 550).
    assert_eq!(messages[2].role, MessageRole::Assistant);
    assert_eq!(messages[2].stats.input_tokens, 550);
    assert_eq!(messages[2].stats.output_tokens, 5);
    assert_eq!(messages[2].stats.cached_tokens, 50);
    assert_eq!(messages[2].stats.tool_calls, 0);

    // Token usage should be non-zero overall (the bug in #190 was zero usage).
    let total_input: u64 = messages.iter().map(|m| m.stats.input_tokens).sum();
    let total_output: u64 = messages.iter().map(|m| m.stats.output_tokens).sum();
    assert!(total_input > 0 && total_output > 0);
}
