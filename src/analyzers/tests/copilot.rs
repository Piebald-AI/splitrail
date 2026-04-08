use crate::analyzer::Analyzer;
use crate::analyzers::copilot::*;
use crate::types::MessageRole;
use std::collections::HashSet;
use std::path::PathBuf;
use tempfile::tempdir;

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
    assert!(
        !patterns_str.contains(".copilot/session-state"),
        "VS Code Copilot patterns should not include Copilot CLI session-state files"
    );
    assert!(
        !patterns_str.contains("events.jsonl"),
        "VS Code Copilot patterns should not include Copilot CLI event files"
    );
}

#[test]
fn test_parse_sample_copilot_cli_session() {
    let temp_dir = tempdir().unwrap();
    let session_dir = temp_dir.path().join("cli-session");
    std::fs::create_dir_all(&session_dir).unwrap();

    let session_file = session_dir.join("events.jsonl");
    std::fs::write(
        &session_file,
        concat!(
            r#"{"type":"session.start","timestamp":"2026-02-09T09:28:30.798Z","data":{"sessionId":"cli-session-1","context":{"cwd":"/home/user/project","model":"openai/gpt-4.1"}}}"#,
            "\n",
            r#"{"type":"user.message","timestamp":"2026-02-09T09:28:31.000Z","data":{"content":"Add a health check endpoint"}}"#,
            "\n",
            r#"{"type":"assistant.message","timestamp":"2026-02-09T09:28:32.000Z","data":{"reasoningText":"I should inspect the server routes.","content":"I'll add the route and wire it up.","toolRequests":[{"toolCallId":"tool-1","toolName":"read_file","arguments":{"path":"src/main.rs"}},{"toolCallId":"tool-2","toolName":"run_in_terminal","arguments":{"command":"cargo test","description":"Run tests"}}]}}"#,
            "\n",
            r#"{"type":"tool.execution_start","timestamp":"2026-02-09T09:28:32.100Z","data":{"toolCallId":"tool-1","toolName":"read_file","arguments":{"path":"src/main.rs"}}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-02-09T09:28:32.200Z","data":{"toolCallId":"tool-1","success":true,"result":{"content":"fn main() {}"}}}"#,
            "\n",
            r#"{"type":"tool.execution_start","timestamp":"2026-02-09T09:28:32.300Z","data":{"toolCallId":"tool-2","toolName":"run_in_terminal","arguments":{"command":"cargo test","description":"Run tests"}}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-02-09T09:28:32.400Z","data":{"toolCallId":"tool-2","success":true,"result":{"content":"test result: ok"}}}"#,
            "\n",
            r#"{"type":"assistant.message.delta","timestamp":"2026-02-09T09:28:33.000Z","data":{"content":"Done — the endpoint is available at /health."}}"#,
            "\n"
        ),
    )
    .unwrap();

    let messages = parse_copilot_cli_session_file(&session_file).unwrap();
    assert_eq!(
        messages.len(),
        2,
        "Expected one user message and one assistant message"
    );

    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[0].model, None);
    assert_eq!(messages[0].stats.input_tokens, 0);
    assert_eq!(messages[0].stats.output_tokens, 0);

    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].model.as_deref(), Some("gpt-4.1"));
    assert_eq!(messages[1].stats.tool_calls, 2);
    assert_eq!(messages[1].stats.files_read, 1);
    assert_eq!(messages[1].stats.terminal_commands, 1);
    assert!(messages[1].stats.input_tokens > 0);
    assert!(messages[1].stats.output_tokens > 0);
    assert_eq!(
        messages[1].session_name.as_deref(),
        Some("Add a health check endpoint")
    );
}
