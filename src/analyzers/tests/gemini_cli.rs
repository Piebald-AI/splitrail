use crate::analyzers::gemini_cli::GeminiCliAnalyzer;
use crate::analyzer::Analyzer;
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;

#[tokio::test]
async fn test_gemini_cli_reasoning_tokens() {
    let dir = tempdir().unwrap();
    let project_dir = dir.path().join("tmp").join("project-123").join("chats");
    std::fs::create_dir_all(&project_dir).unwrap();
    let session_path = project_dir.join("session.json");

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

    let mut file = File::create(&session_path).unwrap();
    file.write_all(json_content.as_bytes()).unwrap();

    let analyzer = GeminiCliAnalyzer::new();
    
    // We can't easily inject sources into `get_stats` without mocking `glob` or `discover_data_sources`.
    // But `parse_conversations` takes a list of sources.
    
    let sources = vec![crate::analyzer::DataSource { path: session_path }];
    let messages = analyzer.parse_conversations(sources).await.unwrap();
    
    assert_eq!(messages.len(), 2);
    
    let assistant_msg = messages.iter().find(|m| m.role == crate::types::MessageRole::Assistant).unwrap();
    assert_eq!(assistant_msg.stats.reasoning_tokens, 123);
    assert_eq!(assistant_msg.stats.input_tokens, 10);
    assert_eq!(assistant_msg.stats.output_tokens, 20);
}
