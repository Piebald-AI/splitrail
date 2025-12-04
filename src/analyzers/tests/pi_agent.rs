use std::io::Write;
use tempfile::NamedTempFile;

use crate::analyzer::{Analyzer, DataSource};
use crate::analyzers::pi_agent::PiAgentAnalyzer;
use crate::types::MessageRole;

#[test]
fn test_pi_agent_analyzer_creation() {
    let analyzer = PiAgentAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Pi Agent");
}

#[test]
fn test_pi_agent_glob_patterns() {
    let analyzer = PiAgentAnalyzer::new();
    let patterns = analyzer.get_data_glob_patterns();
    assert!(!patterns.is_empty());
    assert!(patterns[0].contains(".pi/agent/sessions"));
}

#[test]
fn test_pi_agent_discover_data_sources_no_panic() {
    let analyzer = PiAgentAnalyzer::new();
    // Should not panic, even if directory doesn't exist
    let result = analyzer.discover_data_sources();
    assert!(result.is_ok());
}

#[test]
fn test_pi_agent_is_available() {
    let analyzer = PiAgentAnalyzer::new();
    // Just ensure it doesn't panic
    let _ = analyzer.is_available();
}

#[test]
fn test_parse_pi_agent_session_basic() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session header
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"off"}}"#
    ).unwrap();

    // User message
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:01.000Z","message":{{"role":"user","content":"Hello, Pi!","timestamp":1736935201000}}}}"#
    ).unwrap();

    // Assistant message with usage
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"Hello! How can I help?"}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{{"input":100,"output":50,"cacheRead":20,"cacheWrite":10,"cost":{{"input":0.0003,"output":0.00075,"cacheRead":0.00002,"cacheWrite":0.0000375,"total":0.0011075}}}},"stopReason":"stop","timestamp":1736935202000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    assert_eq!(entry.messages.len(), 2);

    // Check user message
    let user_msg = entry
        .messages
        .iter()
        .find(|m| matches!(m.role, MessageRole::User))
        .unwrap();
    assert!(user_msg.model.is_none());

    // Check assistant message
    let assistant_msg = entry
        .messages
        .iter()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .unwrap();
    assert_eq!(
        assistant_msg.model,
        Some("anthropic/claude-sonnet-4-5".to_string())
    );
    assert_eq!(assistant_msg.stats.input_tokens, 100);
    assert_eq!(assistant_msg.stats.output_tokens, 50);
    assert_eq!(assistant_msg.stats.cache_read_tokens, 20);
    assert_eq!(assistant_msg.stats.cache_creation_tokens, 10);
    assert_eq!(assistant_msg.stats.cached_tokens, 30); // 20 + 10
    assert!((assistant_msg.stats.cost - 0.0011075).abs() < 0.0000001);
}

#[test]
fn test_parse_pi_agent_tool_calls() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session header
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"off"}}"#
    ).unwrap();

    // Assistant message with tool calls
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"Let me read that file."}},{{"type":"toolCall","id":"tool_1","name":"Read","arguments":{{"path":"/test/file.rs"}}}},{{"type":"toolCall","id":"tool_2","name":"Bash","arguments":{{"command":"ls -la"}}}},{{"type":"toolCall","id":"tool_3","name":"Glob","arguments":{{"pattern":"*.rs"}}}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{{"input":100,"output":200,"cacheRead":0,"cacheWrite":0,"cost":{{"total":0.005}}}},"stopReason":"toolUse","timestamp":1736935202000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    let assistant_msg = entry
        .messages
        .iter()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.stats.tool_calls, 3);
    assert_eq!(assistant_msg.stats.files_read, 1);
    assert_eq!(assistant_msg.stats.terminal_commands, 1);
    assert_eq!(assistant_msg.stats.file_searches, 1);
}

#[test]
fn test_parse_pi_agent_model_change() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session header with initial model
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"off"}}"#
    ).unwrap();

    // First assistant message uses session model
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"First response"}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{{"input":50,"output":25,"cacheRead":0,"cacheWrite":0,"cost":{{"total":0.001}}}},"stopReason":"stop","timestamp":1736935202000}}}}"#
    ).unwrap();

    // Model change event
    writeln!(
        temp_file,
        r#"{{"type":"model_change","timestamp":"2025-01-15T10:05:00.000Z","provider":"openai","modelId":"gpt-4o"}}"#
    ).unwrap();

    // Second assistant message uses new model
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:05:02.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"Second response"}}],"api":"openai-completions","provider":"openai","model":"gpt-4o","usage":{{"input":60,"output":30,"cacheRead":0,"cacheWrite":0,"cost":{{"total":0.002}}}},"stopReason":"stop","timestamp":1736935502000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    let assistant_msgs: Vec<_> = entry
        .messages
        .iter()
        .filter(|m| matches!(m.role, MessageRole::Assistant))
        .collect();

    assert_eq!(assistant_msgs.len(), 2);
    assert_eq!(
        assistant_msgs[0].model,
        Some("anthropic/claude-sonnet-4-5".to_string())
    );
    assert_eq!(assistant_msgs[1].model, Some("openai/gpt-4o".to_string()));
}

#[test]
fn test_parse_pi_agent_session_name_from_first_user_message() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session header
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"off"}}"#
    ).unwrap();

    // First user message - should become session name
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:01.000Z","message":{{"role":"user","content":"Help me refactor the authentication module","timestamp":1736935201000}}}}"#
    ).unwrap();

    // Assistant response
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"I'll help you refactor the authentication module."}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{{"input":100,"output":50,"cacheRead":0,"cacheWrite":0,"cost":{{"total":0.002}}}},"stopReason":"stop","timestamp":1736935202000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();

    // All messages should have the session name
    for msg in &entry.messages {
        assert_eq!(
            msg.session_name,
            Some("Help me refactor the authentication module".to_string())
        );
    }
}

#[test]
fn test_parse_pi_agent_session_name_truncation() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session header
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"off"}}"#
    ).unwrap();

    // Long user message - should be truncated to 50 chars + "..."
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:01.000Z","message":{{"role":"user","content":"This is a very long message that should definitely be truncated because it exceeds the maximum length allowed for session names","timestamp":1736935201000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    let session_name = entry.messages[0].session_name.as_ref().unwrap();

    assert!(session_name.ends_with("..."));
    assert_eq!(session_name.chars().count(), 53); // 50 + 3 for "..."
}

#[test]
fn test_parse_pi_agent_different_providers() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session header
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"google","modelId":"gemini-2.5-pro","thinkingLevel":"off"}}"#
    ).unwrap();

    // Google provider message
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"Response from Gemini"}}],"api":"google-generative-ai","provider":"google","model":"gemini-2.5-pro","usage":{{"input":80,"output":40,"cacheRead":15,"cacheWrite":5,"cost":{{"total":0.0015}}}},"stopReason":"stop","timestamp":1736935202000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    let assistant_msg = entry
        .messages
        .iter()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .unwrap();

    assert_eq!(
        assistant_msg.model,
        Some("google/gemini-2.5-pro".to_string())
    );
    assert_eq!(assistant_msg.stats.input_tokens, 80);
    assert_eq!(assistant_msg.stats.output_tokens, 40);
    assert_eq!(assistant_msg.stats.cache_read_tokens, 15);
    assert_eq!(assistant_msg.stats.cache_creation_tokens, 5);
}

#[test]
fn test_parse_pi_agent_thinking_content() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session header
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"high"}}"#
    ).unwrap();

    // Assistant message with thinking block
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"thinking","thinking":"Let me analyze this problem..."}},{{"type":"text","text":"Here's the solution."}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{{"input":100,"output":150,"cacheRead":0,"cacheWrite":0,"cost":{{"total":0.004}}}},"stopReason":"stop","timestamp":1736935202000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    let assistant_msg = entry
        .messages
        .iter()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .unwrap();

    // Should parse without errors even with thinking blocks
    assert_eq!(assistant_msg.stats.output_tokens, 150);
}

#[test]
fn test_parse_pi_agent_edit_and_write_tools() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session header
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"off"}}"#
    ).unwrap();

    // Assistant message with edit and write tools
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"toolCall","id":"tool_1","name":"Edit","arguments":{{"path":"/test/file.rs","old":"foo","new":"bar"}}}},{{"type":"toolCall","id":"tool_2","name":"Write","arguments":{{"path":"/test/new_file.rs","content":"// new file"}}}},{{"type":"toolCall","id":"tool_3","name":"Grep","arguments":{{"pattern":"TODO"}}}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{{"input":100,"output":200,"cacheRead":0,"cacheWrite":0,"cost":{{"total":0.005}}}},"stopReason":"toolUse","timestamp":1736935202000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    let assistant_msg = entry
        .messages
        .iter()
        .find(|m| matches!(m.role, MessageRole::Assistant))
        .unwrap();

    assert_eq!(assistant_msg.stats.tool_calls, 3);
    assert_eq!(assistant_msg.stats.files_edited, 1);
    assert_eq!(assistant_msg.stats.files_added, 1);
    assert_eq!(assistant_msg.stats.file_content_searches, 1);
}

#[test]
fn test_parse_pi_agent_empty_file() {
    let temp_file = NamedTempFile::new().unwrap();
    // File is empty

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    assert!(entry.messages.is_empty());
}

#[test]
fn test_parse_pi_agent_invalid_json_line_skipped() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Valid session header
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"off"}}"#
    ).unwrap();

    // Invalid JSON line (should be skipped)
    writeln!(temp_file, r#"{{ invalid json }}"#).unwrap();

    // Valid assistant message
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"Valid response"}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{{"input":50,"output":25,"cacheRead":0,"cacheWrite":0,"cost":{{"total":0.001}}}},"stopReason":"stop","timestamp":1736935202000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    // Should have parsed the valid message despite the invalid line
    assert_eq!(entry.messages.len(), 1);
}

#[test]
fn test_parse_pi_agent_compaction_entry_ignored() {
    let mut temp_file = NamedTempFile::new().unwrap();

    // Session header
    writeln!(
        temp_file,
        r#"{{"type":"session","id":"test-uuid","timestamp":"2025-01-15T10:00:00.000Z","cwd":"/test/project","provider":"anthropic","modelId":"claude-sonnet-4-5","thinkingLevel":"off"}}"#
    ).unwrap();

    // Compaction entry (should be ignored)
    writeln!(
        temp_file,
        r#"{{"type":"compaction","timestamp":"2025-01-15T10:05:00.000Z","summary":"Previous conversation was about X","firstKeptEntryIndex":5,"tokensBefore":10000}}"#
    ).unwrap();

    // Valid message
    writeln!(
        temp_file,
        r#"{{"type":"message","timestamp":"2025-01-15T10:05:02.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"Continuing..."}}],"api":"anthropic-messages","provider":"anthropic","model":"claude-sonnet-4-5","usage":{{"input":50,"output":25,"cacheRead":0,"cacheWrite":0,"cost":{{"total":0.001}}}},"stopReason":"stop","timestamp":1736935502000}}}}"#
    ).unwrap();

    let analyzer = PiAgentAnalyzer::new();
    let source = DataSource {
        path: temp_file.path().to_path_buf(),
    };
    let result = analyzer.parse_single_file(&source);
    assert!(result.is_ok());

    let entry = result.unwrap();
    // Compaction entry should be skipped, only message should be parsed
    assert_eq!(entry.messages.len(), 1);
}

#[tokio::test]
async fn test_parse_pi_agent_conversations_empty_sources() {
    let analyzer = PiAgentAnalyzer::new();
    let result = analyzer.parse_conversations(vec![]).await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

#[test]
fn test_pi_agent_supports_caching() {
    let analyzer = PiAgentAnalyzer::new();
    assert!(analyzer.supports_caching());
    assert!(analyzer.supports_delta_parsing());
}
