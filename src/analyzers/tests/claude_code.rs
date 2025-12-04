use crate::analyzers::claude_code::{
    TokenFingerprint, calculate_cost_from_tokens, extract_and_hash_project_id, merge_message_into,
    parse_jsonl_file, parse_jsonl_file_delta,
};
use crate::types::{Application, ConversationMessage, MessageRole, Stats};
use chrono::{TimeZone, Utc};
use simd_json::json;
use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Cursor};
use std::path::Path;
use std::sync::LazyLock;

/// Test helper: Sequential deduplication using merge_message_into
fn deduplicate_messages_by_local_hash(
    messages: Vec<ConversationMessage>,
) -> Vec<ConversationMessage> {
    let estimated_unique = messages.len() / 2 + 1;
    let mut seen_hashes = HashMap::<String, usize>::with_capacity(estimated_unique);
    let mut seen_token_fingerprints: HashMap<String, HashSet<TokenFingerprint>> =
        HashMap::with_capacity(estimated_unique);
    let mut deduplicated_entries: Vec<ConversationMessage> = Vec::with_capacity(estimated_unique);

    for message in messages {
        if let Some(local_hash) = &message.local_hash {
            let fp = (
                message.stats.input_tokens,
                message.stats.output_tokens,
                message.stats.cache_creation_tokens,
                message.stats.cache_read_tokens,
                message.stats.cached_tokens,
            );

            if let Some(&existing_index) = seen_hashes.get(local_hash) {
                let seen_fps = seen_token_fingerprints
                    .entry(local_hash.clone())
                    .or_default();
                merge_message_into(
                    &mut deduplicated_entries[existing_index],
                    &message,
                    seen_fps,
                    fp,
                );
            } else {
                seen_hashes.insert(local_hash.clone(), deduplicated_entries.len());
                seen_token_fingerprints
                    .entry(local_hash.clone())
                    .or_default()
                    .insert(fp);
                deduplicated_entries.push(message);
            }
        } else {
            deduplicated_entries.push(message);
        }
    }

    deduplicated_entries
}

// Test data for full conversation parsing
static JSONL_DATA: LazyLock<String> = LazyLock::new(|| {
    r#"{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"D:\\splitrail","sessionId":"502be1cb-cf86-ecce-fe62-89ddec1e7563","version":"1.0.51","type":"user","message":{"role":"user","content":"What is this repo about?"},"uuid":"ba7d3ce9-c931-1a41-836d-a88d85c7aa83","timestamp":"2025-08-02T14:05:11.425Z"}
{"parentUuid":"ba7d3ce9-c931-1a41-836d-a88d85c7aa83","isSidechain":false,"userType":"external","cwd":"D:\\splitrail","sessionId":"502be1cb-cf86-ecce-fe62-89ddec1e7563","version":"1.0.51","message":{"id":"msg_19163d6657d79828b47fd7","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Splitrail is a comprehensive agentic AI coding tool usage analyzer written in Rust."}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":4,"cache_creation_input_tokens":16027,"cache_read_input_tokens":0,"output_tokens":7,"service_tier":"standard"}},"requestId":"req_9d519281655d7bb03077","type":"assistant","uuid":"62b38f0c-18fa-78a3-635f-8b62138ca773","timestamp":"2025-08-02T14:05:17.096Z"}
{"parentUuid":"62b38f0c-18fa-78a3-635f-8b62138ca773","isSidechain":false,"userType":"external","cwd":"D:\\splitrail","sessionId":"502be1cb-cf86-ecce-fe62-89ddec1e7563","version":"1.0.51","message":{"id":"msg_4ed05b6f83dffea6d28e91","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"tool_use","id":"toolu_12345","name":"Read","input":{"file_path":"test.rs"}}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":200,"cache_read_input_tokens":50,"output_tokens":15,"service_tier":"standard"}},"requestId":"req_tool_use_test","type":"assistant","uuid":"tool-use-uuid","timestamp":"2025-08-02T14:05:26.780Z"}
{"parentUuid":"tool-use-uuid","isSidechain":false,"userType":"external","cwd":"D:\\splitrail","sessionId":"502be1cb-cf86-ecce-fe62-89ddec1e7563","version":"1.0.51","type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_12345","type":"tool_result","content":"File contents here"}]},"uuid":"tool-result-uuid","timestamp":"2025-08-02T14:05:30.000Z","toolUseResult":{"type":"text","file":{"filePath":"test.rs","content":"fn main() {}","numLines":1,"startLine":1,"totalLines":1}}}"#.to_string()
});

// Test data with tool operations for extract_tool_stats testing
static TOOL_OPERATIONS_DATA: LazyLock<String> = LazyLock::new(|| {
    r#"{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"D:\\splitrail","sessionId":"test-session","version":"1.0.51","message":{"id":"msg_edit_test","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"tool_use","id":"toolu_edit1","name":"Edit","input":{"file_path":"test.rs","old_string":"old","new_string":"new"}},{"type":"tool_use","id":"toolu_bash1","name":"Bash","input":{"command":"ls -la"}}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":20,"service_tier":"standard"}},"requestId":"req_multi_tool","type":"assistant","uuid":"multi-tool-uuid","timestamp":"2025-08-02T15:00:00.000Z"}
{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"D:\\splitrail","sessionId":"test-session","version":"1.0.51","type":"user","message":{"role":"user","content":[{"tool_use_id":"toolu_todo_write","type":"tool_result","content":"Todo updated"}]},"uuid":"todo-result-uuid","timestamp":"2025-08-02T15:01:00.000Z","toolUseResult":{"oldTodos":[{"id":"1","title":"Task 1","status":"pending","priority":"high"}],"newTodos":[{"id":"1","title":"Task 1","status":"completed","priority":"high"},{"id":"2","title":"Task 2","status":"in_progress","priority":"medium"}]}}"#.to_string()
});

// Test data for duplicate messages - second message has higher token usage
static DUPLICATE_MESSAGES_DATA: LazyLock<String> = LazyLock::new(|| {
    r#"{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"D:\\splitrail","sessionId":"dup-session","version":"1.0.51","message":{"id":"msg_duplicate","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"First message"}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":100,"cache_read_input_tokens":20,"output_tokens":5,"service_tier":"standard"}},"requestId":"req_dup_test","type":"assistant","uuid":"dup-uuid-1","timestamp":"2025-08-02T16:00:00.000Z"}
{"parentUuid":null,"isSidechain":false,"userType":"external","cwd":"D:\\splitrail","sessionId":"dup-session","version":"1.0.51","message":{"id":"msg_duplicate","type":"message","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Second message (duplicate)"}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":15,"cache_creation_input_tokens":150,"cache_read_input_tokens":30,"output_tokens":10,"service_tier":"standard"}},"requestId":"req_dup_test","type":"assistant","uuid":"dup-uuid-2","timestamp":"2025-08-02T16:00:01.000Z"}"#.to_string()
});

#[test]
fn test_parse_jsonl_file_basic() {
    let cursor = Cursor::new(JSONL_DATA.clone());
    let mut buf_reader = BufReader::new(cursor);
    let (messages, _, _, _) = parse_jsonl_file(
        Path::new("test.jsonl"),
        &mut buf_reader,
        "proj_hash",
        "conv_hash",
    )
    .unwrap();

    assert_eq!(messages.len(), 4);

    // Check first message (user message)
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[0].application, Application::ClaudeCode);
    assert_eq!(messages[0].stats.input_tokens, 0);
    assert_eq!(messages[0].stats.output_tokens, 0);

    // Check assistant message with token usage
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].stats.input_tokens, 4);
    assert_eq!(messages[1].stats.cache_creation_tokens, 16027);
    assert_eq!(messages[1].stats.cache_read_tokens, 0);
    assert_eq!(messages[1].stats.output_tokens, 7);
    assert_eq!(
        messages[1].model,
        Some("claude-sonnet-4-20250514".to_string())
    );

    // Check tool use message
    assert_eq!(messages[2].stats.tool_calls, 1);
    assert_eq!(messages[2].stats.files_read, 1);
    assert_eq!(messages[2].stats.input_tokens, 10);
    assert_eq!(messages[2].stats.output_tokens, 15);

    // Check user tool result message
    assert_eq!(messages[3].role, MessageRole::User);
    assert_eq!(messages[3].stats.input_tokens, 0);
    assert_eq!(messages[3].stats.output_tokens, 0);
}

#[test]
fn test_parse_jsonl_file_tool_operations() {
    let cursor = Cursor::new(TOOL_OPERATIONS_DATA.clone());
    let mut buf_reader = BufReader::new(cursor);
    let (messages, _, _, _) = parse_jsonl_file(
        Path::new("tools.jsonl"),
        &mut buf_reader,
        "proj_hash",
        "conv_hash",
    )
    .unwrap();

    assert_eq!(messages.len(), 2);

    // Check multi-tool message
    let multi_tool_msg = &messages[0];
    assert_eq!(multi_tool_msg.stats.tool_calls, 2);
    assert_eq!(multi_tool_msg.stats.files_edited, 1);
    assert_eq!(multi_tool_msg.stats.terminal_commands, 1);

    // Check todo result message
    let todo_msg = &messages[1];
    assert_eq!(todo_msg.role, MessageRole::User);
    assert_eq!(todo_msg.stats.todos_completed, 1);
    assert_eq!(todo_msg.stats.todos_in_progress, 1);
}

#[test]
fn test_extract_and_hash_project_id() {
    let path1 = Path::new("/home/user/.claude/projects/proj123/conversation.jsonl");
    let path2 = Path::new("/home/user/.claude/projects/proj123/other.jsonl");
    let path3 = Path::new("/home/user/.claude/projects/proj456/conversation.jsonl");

    let hash1 = extract_and_hash_project_id(path1);
    let hash2 = extract_and_hash_project_id(path2);
    let hash3 = extract_and_hash_project_id(path3);

    // Same project should have same hash
    assert_eq!(hash1, hash2);
    // Different projects should have different hashes
    assert_ne!(hash1, hash3);
    // Hashes should not be empty
    assert!(!hash1.is_empty());
    assert!(!hash3.is_empty());
}

#[test]
fn test_deduplicate_messages_by_local_hash() {
    let cursor = Cursor::new(DUPLICATE_MESSAGES_DATA.clone());
    let mut buf_reader = BufReader::new(cursor);
    let (messages, _, _, _) = parse_jsonl_file(
        Path::new("duplicates.jsonl"),
        &mut buf_reader,
        "proj_hash",
        "conv_hash",
    )
    .unwrap();

    // Should have 2 messages before deduplication
    assert_eq!(messages.len(), 2);

    let deduplicated = deduplicate_messages_by_local_hash(messages);

    // Deduplication now aggregates messages with the same local_hash
    assert_eq!(deduplicated.len(), 1);

    // Should sum the tokens from both entries: 10 + 15 = 25
    assert_eq!(deduplicated[0].stats.input_tokens, 25);

    // Test with manually created messages that should be deduplicated
    let mut test_messages = Vec::new();

    let base_msg = ConversationMessage {
        global_hash: "global1".to_string(),
        local_hash: Some("local1".to_string()),
        application: Application::ClaudeCode,
        model: Some("test-model".to_string()),
        date: Utc.timestamp_opt(1609459200, 0).unwrap(),
        project_hash: "project1".to_string(),
        conversation_hash: "conv1".to_string(),
        stats: Stats {
            input_tokens: 10,
            output_tokens: 5,
            cache_creation_tokens: 2,
            cache_read_tokens: 1,
            cached_tokens: 3,
            ..Default::default()
        },
        role: MessageRole::Assistant,
        uuid: Some("uuid1".to_string()),
        session_name: Some("Session 1".to_string()),
    };

    let duplicate_msg = ConversationMessage {
        global_hash: "global2".to_string(),
        local_hash: Some("local1".to_string()), // Same local hash
        stats: Stats {
            input_tokens: 15, // Higher token counts than base_msg
            output_tokens: 8,
            cache_creation_tokens: 3,
            cache_read_tokens: 2,
            cached_tokens: 5,
            ..Default::default()
        },
        ..base_msg.clone()
    };

    test_messages.push(base_msg);
    test_messages.push(duplicate_msg.clone());

    let deduplicated_test = deduplicate_messages_by_local_hash(test_messages);
    // Aggregation logic merges messages with the same local_hash
    assert_eq!(deduplicated_test.len(), 1);
    // Should keep the first message's metadata but aggregate the tokens
    assert_eq!(deduplicated_test[0].global_hash, "global1");
    // Should sum the tokens from both entries: 10 + 15 = 25
    assert_eq!(deduplicated_test[0].stats.input_tokens, 25);
    // Should sum output tokens: 5 + 8 = 13
    assert_eq!(deduplicated_test[0].stats.output_tokens, 13);
    // Should sum cache tokens: 3 + 5 = 8
    assert_eq!(deduplicated_test[0].stats.cached_tokens, 8);
    // Should preserve session name
    assert_eq!(
        deduplicated_test[0].session_name,
        Some("Session 1".to_string())
    );
}

#[test]
fn test_parse_jsonl_file_with_summary() {
    let jsonl_data = r#"{"uuid":"msg-uuid-1","type":"user","message":{"role":"user","content":"Hello"},"timestamp":"2025-01-01T00:00:00Z"}
{"type":"summary","summary":"Test Session Summary","leafUuid":"msg-uuid-1"}"#;

    let cursor = Cursor::new(jsonl_data);
    let mut buf_reader = BufReader::new(cursor);
    let (messages, summaries, _, _) = parse_jsonl_file(
        Path::new("summary.jsonl"),
        &mut buf_reader,
        "proj_hash",
        "conv_hash",
    )
    .unwrap();

    assert_eq!(messages.len(), 1);
    assert_eq!(summaries.len(), 1);

    assert_eq!(messages[0].uuid, Some("msg-uuid-1".to_string()));
    assert_eq!(
        summaries.get("msg-uuid-1"),
        Some(&"Test Session Summary".to_string())
    );
}

#[test]
fn test_parse_jsonl_file_fallback_plain_string_content() {
    let jsonl_data = r#"{"uuid":"msg-uuid-1","type":"user","message":{"role":"user","content":"Hello, this is a plain string user message without blocks."},"timestamp":"2025-01-01T00:00:00Z"}"#;

    let cursor = Cursor::new(jsonl_data);
    let mut buf_reader = BufReader::new(cursor);
    let (messages, summaries, _, fallback) = parse_jsonl_file(
        Path::new("fallback_plain.jsonl"),
        &mut buf_reader,
        "proj_hash",
        "conv_hash",
    )
    .unwrap();

    assert_eq!(messages.len(), 1);
    assert!(summaries.is_empty());

    // Fallback must always be populated when there is at least one user message
    assert!(fallback.is_some());
    let name = fallback.unwrap();
    assert!(name.starts_with("Hello, this is a plain string user message"));
    assert!(name.ends_with("..."));
    assert_eq!(name.chars().count(), 53); // 50 chars + "..."
}

#[test]
fn test_parse_jsonl_file_fallback_session_name() {
    let jsonl_data = r#"{"uuid":"msg-uuid-1","type":"user","message":{"role":"user","content":[{"type":"text","text":"This is a long user message that should be truncated for the session name fallback."}]},"timestamp":"2025-01-01T00:00:00Z"}"#;

    let cursor = Cursor::new(jsonl_data);
    let mut buf_reader = BufReader::new(cursor);
    let (messages, summaries, _, fallback) = parse_jsonl_file(
        Path::new("fallback.jsonl"),
        &mut buf_reader,
        "proj_hash",
        "conv_hash",
    )
    .unwrap();

    assert_eq!(messages.len(), 1);
    assert!(summaries.is_empty());

    // Check fallback name
    assert!(fallback.is_some());
    let name = fallback.unwrap();
    assert!(name.starts_with("This is a long user message"));
    assert!(name.ends_with("..."));
    assert_eq!(name.len(), 53); // 50 chars + "..."
}

#[test]
fn test_parse_jsonl_file_fallback_multibyte() {
    // String with emojis to test multi-byte truncation safety
    // "Hello 🌍! " repeated to exceed 50 chars. Each emoji is 4 bytes.
    let text = "Hello 🌍! ".repeat(10);
    let jsonl_data = format!(
        r#"{{"uuid":"msg-uuid-1","type":"user","message":{{"role":"user","content":[{{"type":"text","text":"{}"}}]}},"timestamp":"2025-01-01T00:00:00Z"}}"#,
        text
    );

    let cursor = Cursor::new(jsonl_data);
    let mut buf_reader = BufReader::new(cursor);
    let (messages, summaries, _, fallback) = parse_jsonl_file(
        Path::new("fallback_multibyte.jsonl"),
        &mut buf_reader,
        "proj_hash",
        "conv_hash",
    )
    .unwrap();

    assert_eq!(messages.len(), 1);
    assert!(summaries.is_empty());

    // Check fallback name
    assert!(fallback.is_some());
    let name = fallback.unwrap();
    // Should not panic and should be truncated safely
    assert!(name.ends_with("..."));
    // 50 chars + 3 dots = 53 chars (not bytes)
    assert_eq!(name.chars().count(), 53);
}

#[test]
fn test_calculate_cost_from_tokens() {
    use crate::analyzers::claude_code::Usage;

    let usage = Usage {
        input_tokens: 1000,
        output_tokens: 500,
        cache_creation_input_tokens: 200,
        cache_read_input_tokens: 100,
    };

    let cost = calculate_cost_from_tokens(&usage, "claude-sonnet-4-20250514");

    // Sonnet 4 pricing: $0.003/$0.015 per 1K input/output tokens
    // Cache creation: $0.00375 per 1K tokens, Cache read: $0.0003 per 1K tokens
    let expected_cost = (1000.0 * 0.003 / 1000.0) +  // Input tokens
        (500.0 * 0.015 / 1000.0) +   // Output tokens
        (200.0 * 0.00375 / 1000.0) + // Cache creation
        (100.0 * 0.0003 / 1000.0); // Cache read

    assert!(
        (cost - expected_cost).abs() < 0.0001,
        "Expected cost {expected_cost}, got {cost}"
    );
}

#[test]
fn test_extract_tool_stats_basic_tools() {
    use crate::analyzers::claude_code::{Content, ContentBlock, extract_tool_stats};

    let content = Content::Blocks(vec![
        ContentBlock::ToolUse {
            id: "tool1".to_string(),
            name: "Read".to_string(),
            input: json!({"file_path": "test.rs"}),
        },
        ContentBlock::ToolUse {
            id: "tool2".to_string(),
            name: "Edit".to_string(),
            input: json!({"file_path": "test.rs", "old_string": "old", "new_string": "new"}),
        },
        ContentBlock::ToolUse {
            id: "tool3".to_string(),
            name: "Bash".to_string(),
            input: json!({"command": "ls -la"}),
        },
    ]);

    let stats = extract_tool_stats(&content, &None);

    assert_eq!(stats.files_read, 1);
    assert_eq!(stats.files_edited, 1);
    assert_eq!(stats.terminal_commands, 1);
    assert_eq!(stats.files_added, 0);
    assert_eq!(stats.file_searches, 0);
}

#[test]
fn test_extract_tool_stats_all_tools() {
    use crate::analyzers::claude_code::{Content, ContentBlock, extract_tool_stats};

    let content = Content::Blocks(vec![
        ContentBlock::ToolUse {
            id: "tool1".to_string(),
            name: "Write".to_string(),
            input: json!({"file_path": "new.rs", "content": "fn main() {}"}),
        },
        ContentBlock::ToolUse {
            id: "tool2".to_string(),
            name: "MultiEdit".to_string(),
            input: json!({"file_path": "test.rs", "edits": []}),
        },
        ContentBlock::ToolUse {
            id: "tool3".to_string(),
            name: "Glob".to_string(),
            input: json!({"pattern": "*.rs"}),
        },
        ContentBlock::ToolUse {
            id: "tool4".to_string(),
            name: "Grep".to_string(),
            input: json!({"pattern": "fn main", "path": "."}),
        },
        ContentBlock::ToolUse {
            id: "tool5".to_string(),
            name: "TodoWrite".to_string(),
            input: json!({"todos": []}),
        },
        ContentBlock::ToolUse {
            id: "tool6".to_string(),
            name: "TodoRead".to_string(),
            input: json!({}),
        },
    ]);

    let stats = extract_tool_stats(&content, &None);

    assert_eq!(stats.files_added, 1);
    assert_eq!(stats.files_edited, 1);
    assert_eq!(stats.file_searches, 1);
    assert_eq!(stats.file_content_searches, 1);
    assert_eq!(stats.todo_writes, 1);
    assert_eq!(stats.todo_reads, 1);
}

#[test]
fn test_extract_tool_stats_with_todo_result() {
    use crate::analyzers::claude_code::{Content, extract_tool_stats};

    let tool_result = json!({
        "oldTodos": [
            {"id": "1", "title": "Task 1", "status": "pending", "priority": "high"},
            {"id": "2", "title": "Task 2", "status": "in_progress", "priority": "medium"},
            {"id": "3", "title": "Task 3", "status": "completed", "priority": "low"}
        ],
        "newTodos": [
            {"id": "1", "title": "Task 1", "status": "completed", "priority": "high"},
            {"id": "2", "title": "Task 2", "status": "in_progress", "priority": "medium"},
            {"id": "3", "title": "Task 3", "status": "completed", "priority": "low"},
            {"id": "4", "title": "Task 4", "status": "pending", "priority": "high"},
            {"id": "5", "title": "Task 5", "status": "in_progress", "priority": "medium"}
        ]
    });

    let content = Content::String(serde_bytes::ByteBuf::new());
    let stats = extract_tool_stats(&content, &Some(tool_result));

    // 2 new todos created (4 and 5)
    assert_eq!(stats.todos_created, 2);
    // 1 todo completed (task 1: pending -> completed)
    assert_eq!(stats.todos_completed, 1);
    // 1 todo moved to in_progress (task 5)
    assert_eq!(stats.todos_in_progress, 1);
}

#[test]
fn test_extract_tool_stats_text_content() {
    use crate::analyzers::claude_code::{Content, extract_tool_stats};

    let content = Content::String(serde_bytes::ByteBuf::new());
    let stats = extract_tool_stats(&content, &None);

    // Should be all zeros for text content
    assert_eq!(stats.files_read, 0);
    assert_eq!(stats.files_edited, 0);
    assert_eq!(stats.files_added, 0);
    assert_eq!(stats.terminal_commands, 0);
    assert_eq!(stats.file_searches, 0);
    assert_eq!(stats.file_content_searches, 0);
    assert_eq!(stats.todo_writes, 0);
    assert_eq!(stats.todo_reads, 0);
}

#[test]
fn test_extract_tool_stats_unknown_tools() {
    use crate::analyzers::claude_code::{Content, ContentBlock, extract_tool_stats};

    let content = Content::Blocks(vec![
        ContentBlock::ToolUse {
            id: "tool1".to_string(),
            name: "UnknownTool".to_string(),
            input: json!({"param": "value"}),
        },
        ContentBlock::ToolUse {
            id: "tool2".to_string(),
            name: "AnotherUnknownTool".to_string(),
            input: json!({}),
        },
    ]);

    let stats = extract_tool_stats(&content, &None);

    // Unknown tools should not increment any counters
    assert_eq!(stats.files_read, 0);
    assert_eq!(stats.files_edited, 0);
    assert_eq!(stats.files_added, 0);
    assert_eq!(stats.terminal_commands, 0);
    assert_eq!(stats.file_searches, 0);
    assert_eq!(stats.file_content_searches, 0);
    assert_eq!(stats.todo_writes, 0);
    assert_eq!(stats.todo_reads, 0);
}

// Test data for agent sub-session that starts with assistant message (no user message)
static AGENT_SESSION_DATA: LazyLock<String> = LazyLock::new(|| {
    r#"{"parentUuid":null,"isSidechain":true,"userType":"external","cwd":"/code/test","sessionId":"agent-test-session","version":"2.0.51","agentId":"test-agent","message":{"model":"claude-sonnet-4-5-20250929","id":"msg_agent_001","type":"message","role":"assistant","content":[{"type":"text","text":"I'll start by exploring the codebase to understand its structure."}],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":3,"cache_creation_input_tokens":0,"cache_read_input_tokens":4263,"output_tokens":8,"service_tier":"standard"}},"requestId":"req_agent_test","type":"assistant","uuid":"agent-uuid-001","timestamp":"2025-11-25T00:19:15.622Z"}
{"parentUuid":"agent-uuid-001","isSidechain":true,"userType":"external","cwd":"/code/test","sessionId":"agent-test-session","version":"2.0.51","agentId":"test-agent","message":{"model":"claude-sonnet-4-5-20250929","id":"msg_agent_002","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_glob_001","name":"Glob","input":{"pattern":"**/*.rs"}}],"stop_reason":"tool_use","stop_sequence":null,"usage":{"input_tokens":10,"cache_creation_input_tokens":100,"cache_read_input_tokens":500,"output_tokens":15,"service_tier":"standard"}},"requestId":"req_agent_test2","type":"assistant","uuid":"agent-uuid-002","timestamp":"2025-11-25T00:19:20.000Z"}"#.to_string()
});

#[test]
fn test_parse_agent_session_fallback_name_from_assistant() {
    // Agent sub-sessions start with assistant messages, not user messages.
    // The fallback session name should be extracted from the first message with text content.
    let cursor = Cursor::new(AGENT_SESSION_DATA.clone());
    let mut buf_reader = BufReader::new(cursor);
    let (messages, _, _, fallback_name) = parse_jsonl_file(
        Path::new("agent-test.jsonl"),
        &mut buf_reader,
        "proj_hash",
        "conv_hash",
    )
    .unwrap();

    assert_eq!(messages.len(), 2);

    // First message should be assistant
    assert_eq!(messages[0].role, MessageRole::Assistant);

    // Fallback name should be extracted from the first assistant message's text content
    assert!(
        fallback_name.is_some(),
        "Fallback name should be extracted from first assistant message"
    );
    let name = fallback_name.unwrap();
    assert!(
        name.starts_with("I'll start by exploring the codebase"),
        "Fallback name should start with the first message text, got: {}",
        name
    );
}

// =============================================================================
// DELTA PARSING TESTS - THE FRIGHTENING FLUCTUATION FIX
// =============================================================================
//
// These tests verify that delta parsing handles incomplete lines correctly,
// which is critical for preventing the "10 → 7 → 15 → 12" scary fluctuation
// bug during rapid token streaming.

#[test]
fn test_delta_parsing_incomplete_line_at_eof_does_not_lose_data() {
    // This test simulates the "frightening fluctuation" scenario:
    // When tokens come in fast, the file may be read mid-write with an
    // incomplete JSON line at the end. Delta parsing should:
    // 1. Parse all complete lines successfully
    // 2. NOT advance the offset past the incomplete line
    // 3. Return only the complete messages
    //
    // This ensures stats only go UP, never DOWN during rapid writes.

    use std::io::Write;
    use tempfile::NamedTempFile;

    // Create a temp file with complete lines + an incomplete line at EOF
    let mut temp_file = NamedTempFile::new().expect("create temp file");

    // Write two complete messages
    let complete_line_1 = r#"{"uuid":"msg-1","type":"user","message":{"role":"user","content":"Hello"},"timestamp":"2025-01-01T00:00:00Z"}"#;
    let complete_line_2 = r#"{"uuid":"msg-2","type":"assistant","message":{"id":"msg_test","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Hi there!"}],"usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"requestId":"req_test","timestamp":"2025-01-01T00:00:01Z"}"#;

    // Write complete lines with newlines
    writeln!(temp_file, "{}", complete_line_1).expect("write line 1");
    writeln!(temp_file, "{}", complete_line_2).expect("write line 2");

    // Write an INCOMPLETE line (no newline, simulating mid-write)
    let incomplete_line = r#"{"uuid":"msg-3","type":"assistant","message":{"id":"msg_partial","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"I'm still being wri"#;
    write!(temp_file, "{}", incomplete_line).expect("write incomplete line");
    temp_file.flush().expect("flush");

    let file_path = temp_file.path();
    let file_size = std::fs::metadata(file_path).expect("metadata").len();

    // Parse from the beginning (offset 0)
    let result = parse_jsonl_file_delta(file_path, 0, file_size, "proj_hash", "conv_hash");

    let (messages, final_offset) = result.expect("delta parse should succeed");

    // Should have parsed 2 complete messages (the user and assistant)
    assert_eq!(
        messages.len(),
        2,
        "Should parse exactly 2 complete messages, not the incomplete one"
    );

    // The incomplete line should NOT be parsed
    assert!(
        messages.iter().all(|m| m.uuid != Some("msg-3".to_string())),
        "Incomplete message 'msg-3' should NOT be in the results"
    );

    // CRITICAL: The offset should NOT include the incomplete line!
    // This allows the next delta parse to re-read and complete it.
    let incomplete_line_start = complete_line_1.len() + 1 + complete_line_2.len() + 1;
    assert!(
        final_offset as usize <= incomplete_line_start + 1, // +1 for potential newline handling variance
        "Final offset ({}) should be at or before the incomplete line start ({}). \
         If the offset advanced past the incomplete line, we would LOSE that data! \
         This is the root cause of the 'frightening fluctuation' bug.",
        final_offset,
        incomplete_line_start
    );
}

#[test]
fn test_delta_parsing_complete_file_parses_all() {
    // Verify that when all lines are complete, delta parsing gets everything.

    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut temp_file = NamedTempFile::new().expect("create temp file");

    let line_1 = r#"{"uuid":"msg-1","type":"user","message":{"role":"user","content":"Hello"},"timestamp":"2025-01-01T00:00:00Z"}"#;
    let line_2 = r#"{"uuid":"msg-2","type":"assistant","message":{"id":"msg_test","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Hi!"}],"usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"requestId":"req_test","timestamp":"2025-01-01T00:00:01Z"}"#;

    // Write complete lines WITH trailing newlines
    writeln!(temp_file, "{}", line_1).expect("write line 1");
    writeln!(temp_file, "{}", line_2).expect("write line 2");
    temp_file.flush().expect("flush");

    let file_path = temp_file.path();
    let file_size = std::fs::metadata(file_path).expect("metadata").len();

    let (messages, final_offset) =
        parse_jsonl_file_delta(file_path, 0, file_size, "proj_hash", "conv_hash")
            .expect("delta parse");

    assert_eq!(messages.len(), 2, "Should parse both complete messages");

    // Final offset should be at EOF since all lines are complete
    assert_eq!(
        final_offset, file_size,
        "Offset should be at EOF when all lines are complete"
    );
}

#[test]
fn test_delta_parsing_from_middle_of_file() {
    // Test delta parsing starting from a non-zero offset (incremental update).
    //
    // IMPORTANT: When starting from offset > 0, delta parsing skips to the next
    // newline first (to handle the case where we might start mid-line). So if we
    // want to test incremental parsing, we need at least 3 lines and start at
    // an offset such that after the skip, we still have lines to parse.
    //
    // In practice, this works correctly because last_parsed_offset points to
    // the START of an incomplete line, and we skip that line (which may now be
    // complete) to parse any newly appended lines.

    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut temp_file = NamedTempFile::new().expect("create temp file");

    // Write 3 lines: we'll start at offset 0 but with a specific scenario
    let line_1 = r#"{"uuid":"msg-1","type":"user","message":{"role":"user","content":"First"},"timestamp":"2025-01-01T00:00:00Z"}"#;
    let line_2 = r#"{"uuid":"msg-2","type":"user","message":{"role":"user","content":"Second"},"timestamp":"2025-01-01T00:00:01Z"}"#;
    let line_3 = r#"{"uuid":"msg-3","type":"assistant","message":{"id":"msg_third","role":"assistant","model":"claude-sonnet-4-20250514","content":[{"type":"text","text":"Third!"}],"usage":{"input_tokens":20,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}},"requestId":"req_third","timestamp":"2025-01-01T00:00:02Z"}"#;

    writeln!(temp_file, "{}", line_1).expect("write line 1");
    let offset_after_line_1 = std::fs::metadata(temp_file.path()).expect("metadata").len();

    writeln!(temp_file, "{}", line_2).expect("write line 2");
    writeln!(temp_file, "{}", line_3).expect("write line 3");
    temp_file.flush().expect("flush");

    let file_path = temp_file.path();
    let file_size = std::fs::metadata(file_path).expect("metadata").len();

    // Parse starting AFTER line 1. Delta parsing will:
    // 1. Skip to the next newline (skipping line 2, since we might have started mid-line)
    // 2. Parse line 3
    let (messages, _final_offset) = parse_jsonl_file_delta(
        file_path,
        offset_after_line_1,
        file_size,
        "proj_hash",
        "conv_hash",
    )
    .expect("delta parse");

    // Should get line 3 (line 2 is skipped as a safety measure in case we started mid-line)
    // This conservative behavior ensures we never parse corrupted data.
    assert_eq!(
        messages.len(),
        1,
        "Should parse messages after skipping the first line at the start offset"
    );
    assert_eq!(
        messages[0].uuid,
        Some("msg-3".to_string()),
        "Should parse line 3 (line 2 is skipped as safety)"
    );
}

#[test]
fn test_delta_parsing_detects_truncation() {
    // If a file was truncated between metadata check and parse, delta parsing
    // should fail gracefully so the caller can fall back to full reparse.

    use std::io::Write;
    use tempfile::NamedTempFile;

    let mut temp_file = NamedTempFile::new().expect("create temp file");
    let line = r#"{"uuid":"msg-1","type":"user","message":{"role":"user","content":"Hello"},"timestamp":"2025-01-01T00:00:00Z"}"#;
    writeln!(temp_file, "{}", line).expect("write");
    temp_file.flush().expect("flush");

    let file_path = temp_file.path();
    let actual_size = std::fs::metadata(file_path).expect("metadata").len();

    // Claim the file is bigger than it actually is (simulates truncation after metadata read)
    let claimed_size = actual_size + 1000;

    let result = parse_jsonl_file_delta(file_path, 0, claimed_size, "proj_hash", "conv_hash");

    // Should fail because file was "truncated" (smaller than expected)
    assert!(
        result.is_err(),
        "Delta parse should fail when file is smaller than expected (truncation detected)"
    );
}
