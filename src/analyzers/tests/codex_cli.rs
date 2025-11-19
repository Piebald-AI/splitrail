use std::io::Write;
use tempfile::NamedTempFile;

use crate::analyzer::Analyzer;
use crate::analyzers::codex_cli::*;
use crate::models::calculate_total_cost;

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

    // Session name should prefer the first user message over generic summaries like "auto"
    assert!(result
        .iter()
        .any(|msg| msg.session_name.as_deref() == Some("Hey")));
    // Find the assistant message
    let assistant_msg = result
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-5-codex".to_string()));
    assert_eq!(assistant_msg.stats.input_tokens, 69); // 2629 - 2560 (cached tokens subtracted)
    assert_eq!(assistant_msg.stats.output_tokens, 14); // Codex output_tokens already include reasoning
    assert_eq!(assistant_msg.stats.reasoning_tokens, 0);
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
    assert!(!result.is_empty());

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
fn test_codex_cli_fallback_session_name_from_first_user_message() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Turn context without summary
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.676Z","type":"turn_context","payload":{{"cwd":"/home/test","approval_policy":"on-request","model":"gpt-5-codex"}}}}"#
    )
    .unwrap();

    // User message with input_text
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:36.675Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"This is a Codex CLI session title that should be truncated for display."}}]}}}}"#
    )
    .unwrap();

    let result = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    // Fallback session name should be derived from the first user message
    let names: Vec<String> = result
        .iter()
        .filter_map(|msg| msg.session_name.clone())
        .collect();

    assert!(!names.is_empty());
    let name = &names[0];
    assert!(name.starts_with("This is a Codex CLI session title"));
    assert!(name.ends_with("..."));
    assert_eq!(name.chars().count(), 53); // 50 chars + "..."
}

#[test]
fn test_parse_codex_cli_counts_tool_calls() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:00.000Z","type":"turn_context","payload":{{"model":"gpt-5-codex"}}}}"#
    )
    .unwrap();

    // Simulate user input to start the conversation
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:01.000Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"Do the thing"}}]}}}}"#
    )
    .unwrap();

    // Multiple tool calls, including a duplicate call_id that should be deduplicated
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:02.000Z","type":"response_item","payload":{{"type":"function_call","name":"shell","arguments":"{{\"command\":[\"bash\",\"-lc\",\"echo first\"],\"workdir\":\"/tmp\"}}","call_id":"call_duplicate"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:03.000Z","type":"response_item","payload":{{"type":"function_call","name":"shell","arguments":"{{\"command\":[\"bash\",\"-lc\",\"echo first\"],\"workdir\":\"/tmp\"}}","call_id":"call_duplicate"}}}}"#
    )
    .unwrap();
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:04.000Z","type":"response_item","payload":{{"type":"function_call","name":"shell","arguments":"{{\"command\":[\"bash\",\"-lc\",\"echo second\"],\"workdir\":\"/tmp\"}}","call_id":"call_unique"}}}}"#
    )
    .unwrap();

    // Token usage emitted for the assistant response
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:05.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"last_token_usage":{{"input_tokens":120,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5,"total_tokens":155}}}}}}}}"#
    )
    .unwrap();

    // Assistant message content (ignored for stats but included for completeness)
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:20:06.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"All set!"}}]}}}}"#
    )
    .unwrap();

    let result = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    let assistant_msg = result
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.stats.tool_calls, 2);
    assert_eq!(assistant_msg.stats.input_tokens, 100);
    assert_eq!(assistant_msg.stats.output_tokens, 30);
    assert_eq!(assistant_msg.stats.reasoning_tokens, 5);
}

#[test]
fn test_parse_codex_cli_missing_model() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:27.465Z","type":"session_meta","payload":{{"id":"b51f4ba9-0bf5-40b0-8e70-4f339c4b4f52","timestamp":"2025-09-18T00:16:27.461Z","cwd":"/home/test","originator":"codex_cli_rs","cli_version":"0.38.0","instructions":null,"git":null}}}}"#
    )
    .unwrap();

    // Token usage without any turn context to provide model information
    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:30.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":20,"reasoning_output_tokens":10,"total_tokens":1020}},"last_token_usage":{{"input_tokens":1000,"cached_input_tokens":400,"output_tokens":20,"reasoning_output_tokens":10,"total_tokens":1020}},"model_context_window":272000}}}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T00:16:31.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"No model info available."}}]}}}}"#
    )
    .unwrap();

    let result = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    let assistant_msg = result
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-5".to_string()));
    assert_eq!(assistant_msg.stats.input_tokens, 600); // 1000 - 400 cached
    assert_eq!(assistant_msg.stats.output_tokens, 20);
    assert_eq!(assistant_msg.stats.reasoning_tokens, 10);
    assert_eq!(assistant_msg.stats.cached_tokens, 400);
    let expected_cost = calculate_total_cost("gpt-5", 600, 20, 0, 400);
    assert!((assistant_msg.stats.cost - expected_cost).abs() < f64::EPSILON);
}

#[test]
fn test_parse_codex_cli_turn_context_metadata_model() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T01:00:00.000Z","type":"session_meta","payload":{{"id":"e7b89af6-58bf-4b3e-8118-8cef1cf9f7cd","timestamp":"2025-09-18T01:00:00.000Z","cwd":"/home/test","originator":"codex_cli_rs","cli_version":"0.38.0"}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T01:00:05.000Z","type":"turn_context","payload":{{"metadata":{{"model":"gpt-5-mini"}}}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T01:00:06.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Hello!"}}]}}}}"#
    )
    .unwrap();

    let result = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    let assistant_msg = result
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-5-mini".to_string()));
}

#[test]
fn test_parse_codex_cli_event_model_backfill() {
    let mut temp_file = NamedTempFile::new().unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T02:00:00.000Z","type":"session_meta","payload":{{"id":"95b7e78c-3a23-4f3a-bc06-2ce7acc82941","timestamp":"2025-09-18T02:00:00.000Z"}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T02:00:02.000Z","type":"event_msg","payload":{{"type":"token_count","info":{{"model":"gpt-5-codex","total_token_usage":{{"input_tokens":120,"cached_input_tokens":0,"output_tokens":25,"reasoning_output_tokens":5,"total_tokens":145}}}}}}}}"#
    )
    .unwrap();

    writeln!(
        temp_file,
        r#"{{"timestamp":"2025-09-18T02:00:03.000Z","type":"response_item","payload":{{"type":"message","role":"assistant","content":[{{"type":"output_text","text":"Hi!"}}]}}}}"#
    )
    .unwrap();

    let result = parse_codex_cli_jsonl_file(temp_file.path()).unwrap();

    let assistant_msg = result
        .iter()
        .find(|msg| matches!(msg.role, crate::types::MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.model, Some("gpt-5-codex".to_string()));
    assert_eq!(assistant_msg.stats.input_tokens, 120);
    assert_eq!(assistant_msg.stats.output_tokens, 25);
    assert_eq!(assistant_msg.stats.reasoning_tokens, 5);
    assert_eq!(assistant_msg.stats.cached_tokens, 0);
    let expected_cost = calculate_total_cost("gpt-5-codex", 120, 25, 0, 0);
    assert!((assistant_msg.stats.cost - expected_cost).abs() < f64::EPSILON);
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
