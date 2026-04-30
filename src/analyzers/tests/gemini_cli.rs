use crate::analyzer::Analyzer;
use crate::analyzers::gemini_cli::GeminiCliAnalyzer;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

fn write_session(session_dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(session_dir).unwrap();
    let session_path = session_dir.join("session.json");
    let mut file = File::create(&session_path).unwrap();
    file.write_all(body.as_bytes()).unwrap();
    session_path
}

#[tokio::test]
async fn test_gemini_cli_reasoning_tokens() {
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("tmp").join("project-123").join("chats");
    let json_content = r#"{
        "sessionId": "sess-123",
        "projectHash": "proj-hash",
        "startTime": "2025-11-20T10:00:00Z",
        "lastUpdated": "2025-11-20T10:05:00Z",
        "messages": [
            {
                "type": "user",
                "id": "msg-1",
                "timestamp": "2025-11-20T10:00:00Z",
                "content": "Hello"
            },
            {
                "type": "gemini",
                "id": "msg-2",
                "timestamp": "2025-11-20T10:00:05Z",
                "content": "Hi there",
                "model": "gemini-1.5-pro",
                "tokens": {
                    "input": 10,
                    "output": 20,
                    "thoughts": 123,
                    "cached": 5,
                    "tool": 0,
                    "total": 158
                }
            }
        ]
    }"#;
    let session_path = write_session(&project_dir, json_content);

    let analyzer = GeminiCliAnalyzer::new();

    // Use parse_sources_parallel to parse and deduplicate
    let source = crate::analyzer::DataSource { path: session_path };
    let messages = analyzer.parse_sources_parallel(&[source]);

    assert_eq!(messages.len(), 2);

    let assistant_msg = messages
        .iter()
        .find(|m| m.role == crate::types::MessageRole::Assistant)
        .unwrap();
    assert_eq!(assistant_msg.stats.reasoning_tokens, 123);
    assert_eq!(assistant_msg.stats.input_tokens, 10);
    assert_eq!(assistant_msg.stats.output_tokens, 20);
}

/// Regression test for issue #137: Gemini CLI switched its `content` field
/// from a plain string to a multi-modal `PartListUnion`. The parser must now
/// accept arrays of `Part` objects for any message type (user, gemini,
/// system, error, info).
#[tokio::test]
async fn test_gemini_cli_content_array_of_parts() {
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("tmp").join("project-array").join("chats");
    let json_content = r#"{
        "sessionId": "sess-array",
        "projectHash": "proj-hash",
        "startTime": "2025-11-20T10:00:00Z",
        "lastUpdated": "2025-11-20T10:05:00Z",
        "messages": [
            {
                "type": "user",
                "id": "msg-1",
                "timestamp": "2025-11-20T10:00:00Z",
                "content": [
                    {"text": "Summarise this file"}
                ]
            },
            {
                "type": "gemini",
                "id": "msg-2",
                "timestamp": "2025-11-20T10:00:05Z",
                "content": [
                    {"text": "Sure, here's the summary..."}
                ],
                "model": "gemini-2.5-pro",
                "tokens": {
                    "input": 42,
                    "output": 17,
                    "thoughts": 3,
                    "cached": 0,
                    "tool": 0,
                    "total": 62
                }
            }
        ]
    }"#;
    let session_path = write_session(&project_dir, json_content);

    let analyzer = GeminiCliAnalyzer::new();
    let source = crate::analyzer::DataSource { path: session_path };
    let messages = analyzer.parse_sources_parallel(&[source]);

    // Both messages must parse successfully.
    assert_eq!(messages.len(), 2);

    // The user's first-text is used as a session-name fallback. It must survive
    // the array-of-parts shape.
    let user_msg = messages
        .iter()
        .find(|m| m.role == crate::types::MessageRole::User)
        .unwrap();
    assert_eq!(
        user_msg.session_name.as_deref(),
        Some("Summarise this file")
    );

    // Assistant token counts must still be wired up.
    let assistant_msg = messages
        .iter()
        .find(|m| m.role == crate::types::MessageRole::Assistant)
        .unwrap();
    assert_eq!(assistant_msg.stats.input_tokens, 42);
    assert_eq!(assistant_msg.stats.output_tokens, 17);
}

/// A mixed array with non-text parts (e.g. `inlineData`) must still be
/// accepted — unknown part fields are ignored and the text parts contribute
/// to the session-name fallback.
#[tokio::test]
async fn test_gemini_cli_content_mixed_parts() {
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("tmp").join("project-mixed").join("chats");
    let json_content = r#"{
        "sessionId": "sess-mixed",
        "projectHash": "proj-hash",
        "startTime": "2025-11-20T10:00:00Z",
        "lastUpdated": "2025-11-20T10:05:00Z",
        "messages": [
            {
                "type": "user",
                "id": "msg-1",
                "timestamp": "2025-11-20T10:00:00Z",
                "content": [
                    {"text": "Look at this image: "},
                    {"inlineData": {"mimeType": "image/png", "data": "iVBORw0..."}},
                    {"text": "what is it?"}
                ]
            }
        ]
    }"#;
    let session_path = write_session(&project_dir, json_content);

    let analyzer = GeminiCliAnalyzer::new();
    let source = crate::analyzer::DataSource { path: session_path };
    let messages = analyzer.parse_sources_parallel(&[source]);

    assert_eq!(messages.len(), 1);
    let user_msg = &messages[0];
    assert_eq!(user_msg.role, crate::types::MessageRole::User);
    assert_eq!(
        user_msg.session_name.as_deref(),
        Some("Look at this image: what is it?"),
    );
}

/// `PartListUnion` also allows a single `Part` object (not wrapped in an
/// array). Accept it too.
#[tokio::test]
async fn test_gemini_cli_content_single_part_object() {
    let dir = tempdir().unwrap();
    let project_dir = dir
        .path()
        .join("tmp")
        .join("project-singlepart")
        .join("chats");
    let json_content = r#"{
        "sessionId": "sess-singlepart",
        "projectHash": "proj-hash",
        "startTime": "2025-11-20T10:00:00Z",
        "lastUpdated": "2025-11-20T10:05:00Z",
        "messages": [
            {
                "type": "user",
                "id": "msg-1",
                "timestamp": "2025-11-20T10:00:00Z",
                "content": {"text": "single part hello"}
            }
        ]
    }"#;
    let session_path = write_session(&project_dir, json_content);

    let analyzer = GeminiCliAnalyzer::new();
    let source = crate::analyzer::DataSource { path: session_path };
    let messages = analyzer.parse_sources_parallel(&[source]);

    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].session_name.as_deref(),
        Some("single part hello"),
    );
}

/// A long user-message text must be truncated to the first 50 chars even when
/// it arrives as an array of parts.
#[tokio::test]
async fn test_gemini_cli_content_array_session_name_truncated() {
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("tmp").join("project-trunc").join("chats");
    let json_content = r#"{
        "sessionId": "sess-trunc",
        "projectHash": "proj-hash",
        "startTime": "2025-11-20T10:00:00Z",
        "lastUpdated": "2025-11-20T10:05:00Z",
        "messages": [
            {
                "type": "user",
                "id": "msg-1",
                "timestamp": "2025-11-20T10:00:00Z",
                "content": [
                    {"text": "This prompt is definitely longer than fifty characters by design."}
                ]
            }
        ]
    }"#;
    let session_path = write_session(&project_dir, json_content);

    let analyzer = GeminiCliAnalyzer::new();
    let source = crate::analyzer::DataSource { path: session_path };
    let messages = analyzer.parse_sources_parallel(&[source]);

    assert_eq!(messages.len(), 1);
    let name = messages[0].session_name.as_deref().unwrap();
    // 50 chars plus trailing "..."
    assert!(name.ends_with("..."));
    assert_eq!(
        name,
        "This prompt is definitely longer than fifty charac..."
    );
}

/// An empty/missing/null content field should not crash parsing — the
/// message is still recorded (without a session-name fallback from it).
#[tokio::test]
async fn test_gemini_cli_content_missing_or_null() {
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("tmp").join("project-missing").join("chats");
    let json_content = r#"{
        "sessionId": "sess-missing",
        "projectHash": "proj-hash",
        "startTime": "2025-11-20T10:00:00Z",
        "lastUpdated": "2025-11-20T10:05:00Z",
        "messages": [
            {
                "type": "user",
                "id": "msg-1",
                "timestamp": "2025-11-20T10:00:00Z"
            },
            {
                "type": "user",
                "id": "msg-2",
                "timestamp": "2025-11-20T10:00:01Z",
                "content": null
            },
            {
                "type": "user",
                "id": "msg-3",
                "timestamp": "2025-11-20T10:00:02Z",
                "content": "finally some text"
            }
        ]
    }"#;
    let session_path = write_session(&project_dir, json_content);

    let analyzer = GeminiCliAnalyzer::new();
    let source = crate::analyzer::DataSource { path: session_path };
    let messages = analyzer.parse_sources_parallel(&[source]);

    assert_eq!(messages.len(), 3);
    // The empty/null messages do not contribute a session name; the last one does.
    let names: Vec<_> = messages
        .iter()
        .filter_map(|m| m.session_name.clone())
        .collect();
    assert!(names.iter().any(|n| n == "finally some text"));
}

/// The exact failure case from issue #137: a session containing a user
/// message whose `content` is an array of `{ "text": "..." }` parts.
/// Before the fix this produced:
///   `Serde("invalid type: sequence, expected a string")`.
#[tokio::test]
async fn test_gemini_cli_issue_137_regression() {
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("tmp").join("project-137").join("chats");
    // Schema reproduced from the issue report.
    let json_content = r#"{
        "sessionId": "sess-137",
        "projectHash": "proj-hash",
        "startTime": "2025-11-20T10:00:00Z",
        "lastUpdated": "2025-11-20T10:05:00Z",
        "messages": [
            {
                "type": "user",
                "id": "u-1",
                "timestamp": "2025-11-20T10:00:00Z",
                "content": [
                    {"text": "my prompt..."}
                ]
            },
            {
                "type": "gemini",
                "id": "g-1",
                "timestamp": "2025-11-20T10:00:05Z",
                "content": "response...",
                "model": "gemini-3-pro",
                "tokens": {
                    "input": 1,
                    "output": 2,
                    "thoughts": 0,
                    "cached": 0,
                    "tool": 0,
                    "total": 3
                }
            }
        ]
    }"#;
    let session_path = write_session(&project_dir, json_content);

    // The parse must succeed — prior to the fix, parse_source() returned Err.
    let analyzer = GeminiCliAnalyzer::new();
    let source = crate::analyzer::DataSource { path: session_path };
    let parsed = analyzer
        .parse_source(&source)
        .expect("issue #137 regression: parse_source must accept array-of-parts content");
    assert_eq!(parsed.len(), 2);
}

#[tokio::test]
async fn test_gemini_cli_warning_messages_are_ignored() {
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("tmp").join("project-warning").join("chats");
    let json_content = r#"{
        "sessionId": "sess-warning",
        "projectHash": "proj-hash",
        "startTime": "2026-03-20T08:00:00Z",
        "lastUpdated": "2026-03-20T08:05:00Z",
        "messages": [
            {
                "type": "user",
                "id": "u-1",
                "timestamp": "2026-03-20T08:00:00Z",
                "content": [{"text": "run the tests"}]
            },
            {
                "type": "warning",
                "id": "w-1",
                "timestamp": "2026-03-20T08:00:01Z",
                "content": [{"text": "tool output warning"}]
            },
            {
                "type": "gemini",
                "id": "g-1",
                "timestamp": "2026-03-20T08:00:05Z",
                "content": "done",
                "model": "gemini-3-flash-preview",
                "tokens": {
                    "input": 10,
                    "output": 20,
                    "thoughts": 5,
                    "cached": 0,
                    "tool": 0,
                    "total": 35
                }
            }
        ]
    }"#;
    let session_path = write_session(&project_dir, json_content);

    let analyzer = GeminiCliAnalyzer::new();
    let source = crate::analyzer::DataSource { path: session_path };
    let messages = analyzer
        .parse_source(&source)
        .expect("warning message types should not break parsing");

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, crate::types::MessageRole::User);
    assert_eq!(messages[1].role, crate::types::MessageRole::Assistant);
}

#[tokio::test]
async fn test_gemini_cli_jsonl_latest_message_version_wins() {
    let dir = tempdir().unwrap();
    let session_dir = dir
        .path()
        .join("tmp")
        .join("project-jsonl")
        .join("chats")
        .join("9e43d548-335e-4ad0-b797-4f8bce36e08c");
    std::fs::create_dir_all(&session_dir).unwrap();
    let session_path = session_dir.join("06fhku.jsonl");
    let jsonl_content = r#"{"sessionId":"sess-jsonl","projectHash":"proj-hash","startTime":"2026-04-28T16:10:11.637Z","lastUpdated":"2026-04-28T16:10:11.637Z","kind":"main"}
{"id":"u-1","timestamp":"2026-04-28T16:11:14.988Z","type":"user","content":[{"text":"inspect this cache design"}]}
{"$set":{"lastUpdated":"2026-04-28T16:11:14.989Z"}}
{"id":"g-1","timestamp":"2026-04-28T16:11:38.569Z","type":"gemini","content":"first draft","thoughts":[],"tokens":{"input":20,"output":30,"cached":0,"thoughts":4,"tool":0,"total":54},"model":"gemini-3-flash-preview"}
{"$set":{"lastUpdated":"2026-04-28T16:11:38.569Z"}}
{"id":"g-1","timestamp":"2026-04-28T16:11:38.569Z","type":"gemini","content":"final draft","thoughts":[],"tokens":{"input":20,"output":30,"cached":0,"thoughts":4,"tool":0,"total":54},"model":"gemini-3-flash-preview","toolCalls":[{"id":"call-1","name":"run_shell_command","args":{"command":"rg cache"},"result":[]}]}
"#;
    let mut file = File::create(&session_path).unwrap();
    file.write_all(jsonl_content.as_bytes()).unwrap();

    let analyzer = GeminiCliAnalyzer::new();
    let source = crate::analyzer::DataSource {
        path: session_path.clone(),
    };
    let messages = analyzer
        .parse_source(&source)
        .expect("jsonl sessions should parse successfully");

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, crate::types::MessageRole::User);
    assert_eq!(
        messages[0].session_name.as_deref(),
        Some("inspect this cache design")
    );

    let assistant = messages
        .iter()
        .find(|m| m.role == crate::types::MessageRole::Assistant)
        .unwrap();
    assert_eq!(assistant.stats.input_tokens, 20);
    assert_eq!(assistant.stats.output_tokens, 30);
    assert_eq!(assistant.stats.reasoning_tokens, 4);
    assert_eq!(assistant.stats.tool_calls, 1);
    assert_eq!(assistant.stats.terminal_commands, 1);
    assert!(analyzer.is_valid_data_path(&session_path));
}
