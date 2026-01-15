use crate::analyzer::Analyzer;
use crate::analyzers::copilot::*;
use crate::types::MessageRole;
use std::collections::HashSet;
use std::path::PathBuf;

#[test]
fn test_parse_sample_copilot_session() {
    // Test parsing with the sample.json from the tests directory
    let sample_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("analyzers")
        .join("tests")
        .join("source_data")
        .join("copilot.json");

    if !sample_path.exists() {
        // Skip test if sample file doesn't exist
        return;
    }

    let result = super::super::copilot::parse_copilot_session_file(&sample_path);

    match result {
        Ok(messages) => {
            // Verify we got messages
            assert!(
                !messages.is_empty(),
                "Should parse messages from sample file"
            );

            // Check structure: should have alternating user/assistant messages
            for (idx, msg) in messages.iter().enumerate() {
                if idx % 2 == 0 {
                    assert_eq!(
                        msg.role,
                        MessageRole::User,
                        "Even-indexed messages should be user messages"
                    );
                } else {
                    assert_eq!(
                        msg.role,
                        MessageRole::Assistant,
                        "Odd-indexed messages should be assistant messages"
                    );
                }
            }

            // Verify hash uniqueness
            let mut hashes = HashSet::new();
            for msg in &messages {
                assert!(
                    hashes.insert(msg.global_hash.clone()),
                    "All message hashes should be unique"
                );
            }

            // Verify token counts for each message
            // User messages should have 0 tokens
            assert_eq!(
                messages[0].stats.input_tokens, 0,
                "User message 0 should have 0 input tokens"
            );
            assert_eq!(
                messages[0].stats.output_tokens, 0,
                "User message 0 should have 0 output tokens"
            );

            // Assistant message 1
            assert_eq!(
                messages[1].stats.input_tokens, 11257,
                "Assistant message 11257 input tokens"
            );
            assert_eq!(
                messages[1].stats.output_tokens, 678,
                "Assistant message 678 output tokens"
            );
            assert_eq!(
                messages[1].stats.reasoning_tokens, 0,
                "Assistant message 0 reasoning tokens"
            );
            assert_eq!(
                messages[1].stats.cache_creation_tokens, 0,
                "Assistant message 0 cache creation tokens"
            );
            assert_eq!(
                messages[1].stats.cache_read_tokens, 0,
                "Assistant message 0 cache read tokens"
            );
            assert_eq!(
                messages[1].stats.cached_tokens, 0,
                "Assistant message 0 cached tokens"
            );

            // User message 2
            assert_eq!(
                messages[2].stats.input_tokens, 0,
                "User message 2 should have 0 input tokens"
            );
            assert_eq!(
                messages[2].stats.output_tokens, 0,
                "User message 2 should have 0 output tokens"
            );

            // Assistant message 3
            assert_eq!(
                messages[3].stats.input_tokens, 15995,
                "Assistant message 15995 input tokens"
            );
            assert_eq!(
                messages[3].stats.output_tokens, 1002,
                "Assistant message 1003 output tokens"
            );
            assert_eq!(
                messages[3].stats.reasoning_tokens, 0,
                "Assistant message 0 reasoning tokens"
            );
            assert_eq!(
                messages[3].stats.cache_creation_tokens, 0,
                "Assistant message 0 cache creation tokens"
            );
            assert_eq!(
                messages[3].stats.cache_read_tokens, 0,
                "Assistant message 0 cache read tokens"
            );
            assert_eq!(
                messages[3].stats.cached_tokens, 0,
                "Assistant message 0 cached tokens"
            );

            // User message 4
            assert_eq!(
                messages[4].stats.input_tokens, 0,
                "User message 4 should have 0 input tokens"
            );
            assert_eq!(
                messages[4].stats.output_tokens, 0,
                "User message 4 should have 0 output tokens"
            );

            // Assistant message 5
            assert_eq!(
                messages[5].stats.input_tokens, 12590,
                "Assistant message 12590 input tokens"
            );
            assert_eq!(
                messages[5].stats.output_tokens, 1471,
                "Assistant message 1471 output tokens"
            );
            assert_eq!(
                messages[5].stats.reasoning_tokens, 0,
                "Assistant message 0 reasoning tokens"
            );
            assert_eq!(
                messages[5].stats.cache_creation_tokens, 0,
                "Assistant message 0 cache creation tokens"
            );
            assert_eq!(
                messages[5].stats.cache_read_tokens, 0,
                "Assistant message 0 cache read tokens"
            );
            assert_eq!(
                messages[5].stats.cached_tokens, 0,
                "Assistant message 0 cached tokens"
            );
        }
        Err(e) => {
            panic!("Failed to parse sample Copilot session: {}", e);
        }
    }
}

#[test]
fn test_copilot_analyzer_display_name() {
    let analyzer = CopilotAnalyzer::new();
    assert_eq!(analyzer.display_name(), "GitHub Copilot");
}

#[test]
fn test_copilot_glob_patterns() {
    let analyzer = CopilotAnalyzer::new();
    let patterns = analyzer.get_data_glob_patterns();

    // Should have patterns for multiple editors
    assert!(!patterns.is_empty(), "Should have glob patterns defined");

    // Verify patterns include common locations
    let patterns_str = patterns.join(" ");
    assert!(
        patterns_str.contains("chatSessions"),
        "Patterns should include copilot-chat extension"
    );
}
