use std::io::Write;
use tempfile::NamedTempFile;

use crate::analyzer::Analyzer;
use crate::analyzers::codex_cli::*;

#[test]
fn test_parse_codex_cli_new_wrapper_format() {
    // Create a temporary file with the new wrapper format
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session metadata
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:27.465Z","type":"session_meta","payload":{{"id":"243232f1-a7ab-44e6-b2c3-045b673746ea","timestamp":"2025-09-18T00:16:27.461Z","cwd":"/home/test","originator":"codex_cli_rs","cli_version":"0.38.0","instructions":null,"git":{{"commit_hash":"e4b91cc29da68a6fee1edaf44ce50a64bfbdce63","branch":"main","repository_url":"https://github.com/test/repo.git"}}}}}}"#
    ).unwrap();

    // Turn context with model info
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.676Z","type":"turn_context","payload":{{"cwd":"/home/test","approval_policy":"on-request","model":"gpt-5-codex","summary":"auto"}}}}"#
    ).unwrap();

    // User message
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.675Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"Hey"}}]}}}}"#
    ).unwrap();

    // Token count event
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:38.851Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":2629,"cached_input_tokens":2560,"output_tokens":14,"reasoning_output_tokens":0,"total_tokens":2643}},"last_token_usage":{{"input_tokens":2629,"cached_input_tokens":2560,"output_tokens":14,"reasoning_output_tokens":0,"total_tokens":2643}},"model_context_window":272000}}}}}}"#
    ).unwrap();

    // Assistant message
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:38.852Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Hey! How can I help today?"}}]}}}}"#
    ).unwrap();

    let result = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    // Should have parsed user and assistant messages
    assert!(result.len() >= 2);

    // Find the assistant message
    let assistant_msg = result
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-5-codex".to_string()));
    assert_eq!(assistant_msg.stats.input_tokens, 69); // 2629 - 2560 (cached tokens subtracted)
    assert_eq!(assistant_msg.stats.output_tokens, 14); // output_tokens + reasoning_output_tokens (0)
    assert_eq!(assistant_msg.stats.cached_tokens, 2560);
}

#[test]
fn test_parse_codex_cli_wrapper_format_no_tokens() {
    // Test wrapper format without token information
    let mut temp_file = NamedTempFile::new().unwrap();

    // Turn context with model info
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.676Z","type":"turn_context","payload":{{"model":"gpt-4o"}}}}"#
    ).unwrap();

    // Assistant message without preceding token count
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:38.852Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Hello!"}}]}}}}"#
    ).unwrap();

    let result = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    // Should have parsed the assistant message
    assert!(result.len() >= 1);

    let assistant_msg = result
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-4o".to_string()));
    // Should have default stats when no token info is available
    assert_eq!(assistant_msg.stats.input_tokens, 0);
    assert_eq!(assistant_msg.stats.output_tokens, 0);
}

#[test]
fn test_codex_availability() {
    let analyzer = CodexCliAnalyzer::new();

    println!(
        "Codex CLI data patterns: {:?}",
        analyzer.get_data_glob_patterns()
    );

    let sources = analyzer.discover_data_sources();
    println!("Discovered sources: {:?}", sources);

    let is_available = analyzer.is_available();
    println!("Is available: {}", is_available);

    // For debugging - let's check if the home directory exists
    if let Some(home_dir) = std::env::home_dir() {
        let codex_path = home_dir.join(".codex/sessions");
        println!("Codex path exists: {}", codex_path.exists());
        if codex_path.exists() {
            println!("Contents: {:?}", std::fs::read_dir(&codex_path));
        }
    }

    // Don't assert - just print for debugging
}
