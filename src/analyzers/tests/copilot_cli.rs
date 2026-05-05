use crate::analyzers::copilot_cli::{is_copilot_cli_session_file, parse_copilot_cli_session_file};
use crate::models::calculate_total_cost;
use crate::types::MessageRole;
use std::path::PathBuf;
use tempfile::tempdir;
use tiktoken_rs::get_bpe_from_model;

fn token_count(text: &str) -> u64 {
    get_bpe_from_model("o200k_base")
        .map(|bpe| bpe.encode_with_special_tokens(text).len() as u64)
        .unwrap_or_else(|_| (text.len() / 4) as u64)
}

#[test]
fn test_registry_exposes_separate_copilot_cli_analyzer() {
    let registry = crate::create_analyzer_registry();

    let copilot = registry
        .get_analyzer_by_display_name("GitHub Copilot")
        .expect("registry should keep the VS Code Copilot analyzer");
    let copilot_patterns = copilot.get_data_glob_patterns().join(" ");
    assert!(copilot_patterns.contains("chatSessions"));
    assert!(!copilot_patterns.contains(".copilot/session-state"));

    let copilot_cli = registry
        .get_analyzer_by_display_name("GitHub Copilot CLI")
        .expect("registry should register a dedicated Copilot CLI analyzer");
    let cli_patterns = copilot_cli.get_data_glob_patterns().join(" ");
    assert!(cli_patterns.contains(".copilot/session-state"));
    assert!(cli_patterns.contains("events.jsonl"));
    assert!(!cli_patterns.contains("chatSessions"));
}

#[test]
fn test_copilot_cli_identifies_valid_session_files() {
    let nested_path = PathBuf::from("/home/user/.copilot/session-state/12345678-1234/events.jsonl");
    assert!(is_copilot_cli_session_file(&nested_path));

    let flat_path = PathBuf::from("/home/user/.copilot/history-session-state/test.jsonl");
    assert!(is_copilot_cli_session_file(&flat_path));

    let invalid_path = PathBuf::from("/home/user/.copilot/session-state/12345678-1234/meta.json");
    assert!(!is_copilot_cli_session_file(&invalid_path));
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

#[test]
fn test_copilot_cli_messages_use_copilot_cli_application_variant() {
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
            r#"{"type":"assistant.message","timestamp":"2026-02-09T09:28:32.000Z","data":{"reasoningText":"I should inspect the server routes.","content":"I'll add the route and wire it up.","toolRequests":[{"toolCallId":"tool-1","toolName":"read_file","arguments":{"path":"src/main.rs"}}]}}"#,
            "\n",
            r#"{"type":"tool.execution_start","timestamp":"2026-02-09T09:28:32.100Z","data":{"toolCallId":"tool-1","toolName":"read_file","arguments":{"path":"src/main.rs"}}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-02-09T09:28:32.200Z","data":{"toolCallId":"tool-1","success":true,"result":{"content":"fn main() {}"}}}"#,
            "\n",
            r#"{"type":"assistant.message.delta","timestamp":"2026-02-09T09:28:33.000Z","data":{"content":"Done — the endpoint is available at /health."}}"#,
            "\n"
        ),
    )
    .unwrap();

    let messages = parse_copilot_cli_session_file(&session_file).unwrap();
    assert_eq!(messages.len(), 2);

    let user_application =
        String::from_utf8(simd_json::to_vec(&messages[0].application).unwrap()).unwrap();
    let assistant_application =
        String::from_utf8(simd_json::to_vec(&messages[1].application).unwrap()).unwrap();

    assert_eq!(user_application, "\"copilot_cli\"");
    assert_eq!(assistant_application, "\"copilot_cli\"");
}

#[test]
fn test_copilot_cli_uses_shutdown_metrics_for_multi_turn_sessions() {
    let temp_dir = tempdir().unwrap();
    let session_dir = temp_dir.path().join("cli-session");
    std::fs::create_dir_all(&session_dir).unwrap();

    let session_file = session_dir.join("events.jsonl");
    std::fs::write(
        &session_file,
        concat!(
            r#"{"type":"session.start","timestamp":"2026-04-08T05:00:00.000Z","data":{"sessionId":"cli-session-usage","context":{"cwd":"/home/user/project","model":"openai/gpt-5.4"}}}"#,
            "\n",
            r#"{"type":"user.message","timestamp":"2026-04-08T05:00:01.000Z","data":{"content":"Improve the Copilot CLI parser"}}"#,
            "\n",
            r#"{"type":"assistant.turn_start","timestamp":"2026-04-08T05:00:02.000Z","data":{"turnId":"0","interactionId":"interaction-1"}}"#,
            "\n",
            r#"{"type":"assistant.message","timestamp":"2026-04-08T05:00:03.000Z","data":{"messageId":"assistant-1","interactionId":"interaction-1","content":"I'll inspect the current parser.","outputTokens":32700,"toolRequests":[{"toolCallId":"tool-1","toolName":"read_file","arguments":{"path":"src/analyzers/copilot.rs"}}]}}"#,
            "\n",
            r#"{"type":"tool.execution_start","timestamp":"2026-04-08T05:00:04.000Z","data":{"toolCallId":"tool-1","toolName":"read_file","arguments":{"path":"src/analyzers/copilot.rs"}}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-04-08T05:00:05.000Z","data":{"toolCallId":"tool-1","success":true,"result":{"content":"pub(crate) fn parse_copilot_cli_session_file(...)"}}}"#,
            "\n",
            r#"{"type":"assistant.turn_end","timestamp":"2026-04-08T05:00:06.000Z","data":{"turnId":"0"}}"#,
            "\n",
            r#"{"type":"session.model_change","timestamp":"2026-04-08T05:00:07.000Z","data":{"newModel":"anthropic/claude-sonnet-4.5"}}"#,
            "\n",
            r#"{"type":"assistant.turn_start","timestamp":"2026-04-08T05:00:08.000Z","data":{"turnId":"1","interactionId":"interaction-1"}}"#,
            "\n",
            r#"{"type":"assistant.message","timestamp":"2026-04-08T05:00:09.000Z","data":{"messageId":"assistant-2","interactionId":"interaction-1","content":"Now I'll split the CLI analyzer out.","outputTokens":8700,"toolRequests":[{"toolCallId":"tool-2","toolName":"bash","arguments":{"command":"cargo test","description":"Run tests"}}]}}"#,
            "\n",
            r#"{"type":"tool.execution_start","timestamp":"2026-04-08T05:00:10.000Z","data":{"toolCallId":"tool-2","toolName":"bash","arguments":{"command":"cargo test","description":"Run tests"}}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-04-08T05:00:11.000Z","data":{"toolCallId":"tool-2","success":true,"result":{"content":"test result: ok"}}}"#,
            "\n",
            r#"{"type":"assistant.turn_end","timestamp":"2026-04-08T05:00:12.000Z","data":{"turnId":"1"}}"#,
            "\n",
            r#"{"type":"session.shutdown","timestamp":"2026-04-08T05:00:13.000Z","data":{"shutdownType":"routine","totalPremiumRequests":1,"totalApiDurationMs":811000,"sessionStartTime":1775624400000,"modelMetrics":{"gpt-5.4":{"requests":{"count":1,"cost":1},"usage":{"inputTokens":4700000,"outputTokens":32700,"cacheReadTokens":4600000,"cacheWriteTokens":0}},"claude-sonnet-4.5":{"requests":{"count":1,"cost":0},"usage":{"inputTokens":2300000,"outputTokens":8700,"cacheReadTokens":2200000,"cacheWriteTokens":0}}},"currentModel":"claude-sonnet-4.5","currentTokens":152434,"systemTokens":11351,"conversationTokens":86337,"toolDefinitionsTokens":23939}}"#,
            "\n"
        ),
    )
    .unwrap();

    let messages = parse_copilot_cli_session_file(&session_file).unwrap();

    assert_eq!(
        messages.len(),
        3,
        "Expected one user message plus one assistant message per assistant turn"
    );

    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[2].role, MessageRole::Assistant);

    assert_eq!(messages[1].model.as_deref(), Some("gpt-5.4"));
    assert_eq!(messages[1].stats.input_tokens, 4_700_000);
    assert_eq!(messages[1].stats.output_tokens, 32_700);
    assert_eq!(messages[1].stats.cache_read_tokens, 4_600_000);
    assert_eq!(messages[1].stats.tool_calls, 1);
    assert_eq!(messages[1].stats.files_read, 1);
    let expected_gpt_cost = calculate_total_cost("gpt-5.4", 100_000, 32_700, 0, 4_600_000);
    assert!((messages[1].stats.cost - expected_gpt_cost).abs() < f64::EPSILON);

    assert_eq!(messages[2].model.as_deref(), Some("claude-sonnet-4.5"));
    assert_eq!(messages[2].stats.input_tokens, 2_300_000);
    assert_eq!(messages[2].stats.output_tokens, 8_700);
    assert_eq!(messages[2].stats.cache_read_tokens, 2_200_000);
    assert_eq!(messages[2].stats.tool_calls, 1);
    assert_eq!(messages[2].stats.terminal_commands, 1);
    let expected_claude_cost =
        calculate_total_cost("claude-sonnet-4.5", 100_000, 8_700, 0, 2_200_000);
    assert!((messages[2].stats.cost - expected_claude_cost).abs() < f64::EPSILON);
}

#[test]
fn test_copilot_cli_accumulates_multiple_shutdown_segments_in_one_session_file() {
    let temp_dir = tempdir().unwrap();
    let session_dir = temp_dir.path().join("cli-session");
    std::fs::create_dir_all(&session_dir).unwrap();

    let session_file = session_dir.join("events.jsonl");
    std::fs::write(
        &session_file,
        concat!(
            r#"{"type":"session.start","timestamp":"2026-04-08T05:00:00.000Z","data":{"sessionId":"cli-session-resume","context":{"cwd":"/home/user/project","model":"openai/gpt-5.4"}}}"#,
            "\n",
            r#"{"type":"user.message","timestamp":"2026-04-08T05:00:01.000Z","data":{"content":"Handle the first request"}}"#,
            "\n",
            r#"{"type":"assistant.turn_start","timestamp":"2026-04-08T05:00:02.000Z","data":{"turnId":"0","interactionId":"interaction-1"}}"#,
            "\n",
            r#"{"type":"assistant.message","timestamp":"2026-04-08T05:00:03.000Z","data":{"messageId":"assistant-1","interactionId":"interaction-1","content":"Finished the first chunk.","outputTokens":1200}}"#,
            "\n",
            r#"{"type":"assistant.turn_end","timestamp":"2026-04-08T05:00:04.000Z","data":{"turnId":"0"}}"#,
            "\n",
            r#"{"type":"session.shutdown","timestamp":"2026-04-08T05:00:05.000Z","data":{"shutdownType":"routine","totalPremiumRequests":1,"modelMetrics":{"gpt-5.4":{"requests":{"count":1,"cost":0},"usage":{"inputTokens":2000000,"outputTokens":1200,"cacheReadTokens":1800000,"cacheWriteTokens":0}}}}}"#,
            "\n",
            r#"{"type":"user.message","timestamp":"2026-04-08T05:00:06.000Z","data":{"content":"Resume and finish the second request"}}"#,
            "\n",
            r#"{"type":"assistant.turn_start","timestamp":"2026-04-08T05:00:07.000Z","data":{"turnId":"1","interactionId":"interaction-2"}}"#,
            "\n",
            r#"{"type":"assistant.message","timestamp":"2026-04-08T05:00:08.000Z","data":{"messageId":"assistant-2","interactionId":"interaction-2","content":"Finished the second chunk.","outputTokens":3400}}"#,
            "\n",
            r#"{"type":"assistant.turn_end","timestamp":"2026-04-08T05:00:09.000Z","data":{"turnId":"1"}}"#,
            "\n",
            r#"{"type":"session.shutdown","timestamp":"2026-04-08T05:00:10.000Z","data":{"shutdownType":"routine","totalPremiumRequests":1,"modelMetrics":{"gpt-5.4":{"requests":{"count":1,"cost":0},"usage":{"inputTokens":5000000,"outputTokens":3400,"cacheReadTokens":4700000,"cacheWriteTokens":0}}}}}"#,
            "\n"
        ),
    )
    .unwrap();

    let messages = parse_copilot_cli_session_file(&session_file).unwrap();

    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[2].role, MessageRole::User);
    assert_eq!(messages[3].role, MessageRole::Assistant);

    assert_eq!(messages[1].stats.input_tokens, 2_000_000);
    assert_eq!(messages[1].stats.output_tokens, 1_200);
    assert_eq!(messages[1].stats.cache_read_tokens, 1_800_000);

    assert_eq!(messages[3].stats.input_tokens, 5_000_000);
    assert_eq!(messages[3].stats.output_tokens, 3_400);
    assert_eq!(messages[3].stats.cache_read_tokens, 4_700_000);

    let total_input: u64 = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(|message| message.stats.input_tokens)
        .sum();
    let total_output: u64 = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(|message| message.stats.output_tokens)
        .sum();
    let total_cache_read: u64 = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(|message| message.stats.cache_read_tokens)
        .sum();

    assert_eq!(total_input, 7_000_000);
    assert_eq!(total_output, 4_600);
    assert_eq!(total_cache_read, 6_500_000);
}

#[test]
fn test_copilot_cli_keeps_segment_open_when_shutdown_metrics_are_empty() {
    let temp_dir = tempdir().unwrap();
    let session_dir = temp_dir.path().join("cli-session");
    std::fs::create_dir_all(&session_dir).unwrap();

    let session_file = session_dir.join("events.jsonl");
    std::fs::write(
        &session_file,
        concat!(
            r#"{"type":"session.start","timestamp":"2026-04-08T05:00:00.000Z","data":{"sessionId":"cli-session-empty-shutdown","context":{"cwd":"/home/user/project","model":"openai/gpt-5.4"}}}"#,
            "\n",
            r#"{"type":"user.message","timestamp":"2026-04-08T05:00:01.000Z","data":{"content":"Handle the first request"}}"#,
            "\n",
            r#"{"type":"assistant.turn_start","timestamp":"2026-04-08T05:00:02.000Z","data":{"turnId":"0","interactionId":"interaction-1"}}"#,
            "\n",
            r#"{"type":"assistant.message","timestamp":"2026-04-08T05:00:03.000Z","data":{"messageId":"assistant-1","interactionId":"interaction-1","content":"Finished the first chunk.","outputTokens":1200}}"#,
            "\n",
            r#"{"type":"assistant.turn_end","timestamp":"2026-04-08T05:00:04.000Z","data":{"turnId":"0"}}"#,
            "\n",
            r#"{"type":"session.shutdown","timestamp":"2026-04-08T05:00:05.000Z","data":{"shutdownType":"routine","totalPremiumRequests":0}}"#,
            "\n",
            r#"{"type":"user.message","timestamp":"2026-04-08T05:00:06.000Z","data":{"content":"Resume and finish the second request"}}"#,
            "\n",
            r#"{"type":"assistant.turn_start","timestamp":"2026-04-08T05:00:07.000Z","data":{"turnId":"1","interactionId":"interaction-2"}}"#,
            "\n",
            r#"{"type":"assistant.message","timestamp":"2026-04-08T05:00:08.000Z","data":{"messageId":"assistant-2","interactionId":"interaction-2","content":"Finished the second chunk.","outputTokens":3400}}"#,
            "\n",
            r#"{"type":"assistant.turn_end","timestamp":"2026-04-08T05:00:09.000Z","data":{"turnId":"1"}}"#,
            "\n",
            r#"{"type":"session.shutdown","timestamp":"2026-04-08T05:00:10.000Z","data":{"shutdownType":"routine","totalPremiumRequests":1,"modelMetrics":{"gpt-5.4":{"requests":{"count":2,"cost":0},"usage":{"inputTokens":7000000,"outputTokens":4600,"cacheReadTokens":6500000,"cacheWriteTokens":0}}}}}"#,
            "\n"
        ),
    )
    .unwrap();

    let messages = parse_copilot_cli_session_file(&session_file).unwrap();

    assert_eq!(messages.len(), 4);
    assert_eq!(messages[1].stats.input_tokens, 1_826_086);
    assert_eq!(messages[1].stats.output_tokens, 1_200);
    assert_eq!(messages[1].stats.cache_read_tokens, 1_695_652);

    assert_eq!(messages[3].stats.input_tokens, 5_173_914);
    assert_eq!(messages[3].stats.output_tokens, 3_400);
    assert_eq!(messages[3].stats.cache_read_tokens, 4_804_348);

    let total_input: u64 = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(|message| message.stats.input_tokens)
        .sum();
    let total_output: u64 = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(|message| message.stats.output_tokens)
        .sum();
    let total_cache_read: u64 = messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
        .map(|message| message.stats.cache_read_tokens)
        .sum();

    assert_eq!(total_input, 7_000_000);
    assert_eq!(total_output, 4_600);
    assert_eq!(total_cache_read, 6_500_000);
}

#[test]
fn test_copilot_cli_infers_model_from_tool_execution_results() {
    let temp_dir = tempdir().unwrap();
    let session_dir = temp_dir.path().join("cli-session");
    std::fs::create_dir_all(&session_dir).unwrap();

    let session_file = session_dir.join("events.jsonl");
    std::fs::write(
        &session_file,
        concat!(
            r#"{"type":"session.start","timestamp":"2026-04-07T11:58:18.193Z","data":{"sessionId":"cli-session-model","context":{"cwd":"/home/user/project"}}}"#,
            "\n",
            r#"{"type":"user.message","timestamp":"2026-04-07T11:59:29.457Z","data":{"content":"Review this PR"}}"#,
            "\n",
            r#"{"type":"assistant.turn_start","timestamp":"2026-04-07T11:59:29.476Z","data":{"turnId":"0","interactionId":"interaction-1"}}"#,
            "\n",
            r#"{"type":"assistant.message","timestamp":"2026-04-07T11:59:53.444Z","data":{"messageId":"assistant-1","interactionId":"interaction-1","content":"","outputTokens":536,"toolRequests":[{"toolCallId":"tool-1","name":"skill","arguments":{"skill":"pr-review"}}]}}"#,
            "\n",
            r#"{"type":"tool.execution_start","timestamp":"2026-04-07T11:59:53.444Z","data":{"toolCallId":"tool-1","toolName":"skill","arguments":{"skill":"pr-review"}}}"#,
            "\n",
            r#"{"type":"tool.execution_complete","timestamp":"2026-04-07T11:59:53.472Z","data":{"toolCallId":"tool-1","model":"gpt-5.4","interactionId":"interaction-1","success":true,"result":{"content":"Skill loaded"}}}"#,
            "\n",
            r#"{"type":"assistant.turn_end","timestamp":"2026-04-07T11:59:53.475Z","data":{"turnId":"0"}}"#,
            "\n"
        ),
    )
    .unwrap();

    let messages = parse_copilot_cli_session_file(&session_file).unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].model.as_deref(), Some("gpt-5.4"));
    assert_eq!(messages[1].stats.output_tokens, 536);
    assert_eq!(messages[1].stats.tool_calls, 1);
}

#[test]
fn test_copilot_cli_reasoning_text_populates_reasoning_tokens() {
    let temp_dir = tempdir().unwrap();
    let session_dir = temp_dir.path().join("cli-session");
    std::fs::create_dir_all(&session_dir).unwrap();

    let reasoning_text = "I should inspect the current parser, compare shutdown metrics, and keep output accounting unchanged.";
    let session_file = session_dir.join("events.jsonl");
    std::fs::write(
        &session_file,
        format!(
            r#"{{"type":"session.start","timestamp":"2026-04-08T05:00:00.000Z","data":{{"sessionId":"cli-session-reasoning","context":{{"cwd":"/home/user/project","model":"openai/gpt-5.4"}}}}}}
{{"type":"user.message","timestamp":"2026-04-08T05:00:01.000Z","data":{{"content":"Inspect the parser"}}}}
{{"type":"assistant.turn_start","timestamp":"2026-04-08T05:00:02.000Z","data":{{"turnId":"0","interactionId":"interaction-1"}}}}
{{"type":"assistant.message","timestamp":"2026-04-08T05:00:03.000Z","data":{{"messageId":"assistant-1","interactionId":"interaction-1","reasoningText":"{reasoning_text}","content":"I found the parser entrypoint.","outputTokens":1234}}}}
{{"type":"assistant.turn_end","timestamp":"2026-04-08T05:00:04.000Z","data":{{"turnId":"0"}}}}
"#
        ),
    )
    .unwrap();

    let messages = parse_copilot_cli_session_file(&session_file).unwrap();
    assert_eq!(messages.len(), 2);

    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].stats.output_tokens, 1_234);
    assert_eq!(
        messages[1].stats.reasoning_tokens,
        token_count(reasoning_text)
    );
    assert!(messages[1].stats.reasoning_tokens > 0);
}

#[test]
fn test_copilot_cli_live_sessions_estimate_input_from_accumulated_context() {
    let temp_dir = tempdir().unwrap();
    let session_dir = temp_dir.path().join("cli-session");
    std::fs::create_dir_all(&session_dir).unwrap();

    let session_file = session_dir.join("events.jsonl");
    let long_context = "analysis ".repeat(400);
    std::fs::write(
        &session_file,
        format!(
            r#"{{"type":"session.start","timestamp":"2026-04-08T05:00:00.000Z","data":{{"sessionId":"cli-session-live","context":{{"cwd":"/home/user/project","model":"openai/gpt-5.4"}}}}}}
{{"type":"user.message","timestamp":"2026-04-08T05:00:01.000Z","data":{{"content":"Keep iterating on the parser until the live totals look right."}}}}
{{"type":"assistant.turn_start","timestamp":"2026-04-08T05:00:02.000Z","data":{{"turnId":"0","interactionId":"interaction-1"}}}}
{{"type":"assistant.message","timestamp":"2026-04-08T05:00:03.000Z","data":{{"messageId":"assistant-1","interactionId":"interaction-1","content":"{long_context}","outputTokens":3200}}}}
{{"type":"assistant.turn_end","timestamp":"2026-04-08T05:00:04.000Z","data":{{"turnId":"0"}}}}
{{"type":"assistant.turn_start","timestamp":"2026-04-08T05:00:05.000Z","data":{{"turnId":"1","interactionId":"interaction-1"}}}}
{{"type":"assistant.message","timestamp":"2026-04-08T05:00:06.000Z","data":{{"messageId":"assistant-2","interactionId":"interaction-1","content":"Implemented the remaining parser changes.","outputTokens":1200}}}}
{{"type":"assistant.turn_end","timestamp":"2026-04-08T05:00:07.000Z","data":{{"turnId":"1"}}}}
"#
        ),
    )
    .unwrap();

    let messages = parse_copilot_cli_session_file(&session_file).unwrap();

    assert_eq!(messages.len(), 3);
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[2].role, MessageRole::Assistant);
    assert!(
        messages[2].stats.input_tokens > messages[1].stats.input_tokens,
        "live sessions should carry prior assistant context into subsequent turn input estimates"
    );
    assert!(
        messages[2].stats.cache_read_tokens > 0,
        "live sessions should expose a non-zero cached prefix estimate once the conversation has prior context"
    );
}
