use crate::analyzer::{Analyzer, DataSource};
use crate::analyzers::zoo_code::ZooCodeAnalyzer;
use crate::types::{Application, MessageRole};
use std::fs;
use tempfile::tempdir;

#[test]
fn test_zoo_code_analyzer_creation() {
    let analyzer = ZooCodeAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Zoo Code");
}

#[test]
fn test_zoo_code_glob_patterns_use_zoo_extension_id() {
    let analyzer = ZooCodeAnalyzer::new();
    let patterns = analyzer.get_data_glob_patterns();

    assert!(!patterns.is_empty());
    assert!(
        patterns
            .iter()
            .all(|pattern| pattern.contains("zoocodeorganization.zoo-code"))
    );
}

#[test]
fn test_zoo_code_is_available() {
    let analyzer = ZooCodeAnalyzer::new();
    let _ = analyzer.is_available();
}

#[test]
fn test_zoo_code_discover_data_sources_no_panic() {
    let analyzer = ZooCodeAnalyzer::new();
    assert!(analyzer.discover_data_sources().is_ok());
}

#[test]
fn test_zoo_code_parses_roo_format_as_zoo_code() {
    let temp_dir = tempdir().unwrap();
    let task_dir = temp_dir
        .path()
        .join("zoocodeorganization.zoo-code/tasks/task-123");
    fs::create_dir_all(&task_dir).unwrap();
    fs::write(
        task_dir.join("api_conversation_history.json"),
        r#"[{"role":"user","content":[{"type":"text","text":"<environment_details><model>claude-sonnet-4</model></environment_details>"}]}]"#,
    )
    .unwrap();
    fs::write(
        task_dir.join("ui_messages.json"),
        r#"[
            {"type":"ask","ts":1710000000000,"ask":"followup","text":"Add Zoo Code support"},
            {"type":"say","ts":1710000001000,"say":"api_req_started","text":"{\"apiProtocol\":\"anthropic\",\"tokensIn\":120,\"tokensOut\":30,\"cacheWrites\":10,\"cacheReads\":20,\"cost\":0.0123}"}
        ]"#,
    )
    .unwrap();

    let analyzer = ZooCodeAnalyzer::new();
    let messages = analyzer
        .parse_source(&DataSource { path: task_dir })
        .unwrap();

    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].application, Application::ZooCode);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(
        messages[0].session_name.as_deref(),
        Some("Add Zoo Code support")
    );
    assert_eq!(messages[1].application, Application::ZooCode);
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[1].model.as_deref(), Some("claude-sonnet-4"));
    assert_eq!(messages[1].stats.input_tokens, 120);
    assert_eq!(messages[1].stats.output_tokens, 30);
    assert_eq!(messages[1].stats.cache_creation_tokens, 10);
    assert_eq!(messages[1].stats.cache_read_tokens, 20);
    assert_eq!(messages[1].stats.cached_tokens, 30);
    assert_eq!(messages[1].stats.cost, 0.0123);
}

#[tokio::test]
async fn test_zoo_code_get_stats_empty_sources() {
    let analyzer = ZooCodeAnalyzer::new();
    let result = analyzer.get_stats_with_sources(vec![]).unwrap();
    assert!(result.messages.is_empty());
}
