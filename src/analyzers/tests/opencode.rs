use crate::analyzer::Analyzer;
use crate::analyzers::opencode::OpenCodeAnalyzer;

#[test]
fn test_opencode_analyzer_creation() {
    let analyzer = OpenCodeAnalyzer::new();
    assert_eq!(analyzer.display_name(), "OpenCode");
}

#[test]
fn test_opencode_is_available() {
    let analyzer = OpenCodeAnalyzer::new();
    // is_available depends on whether OpenCode data exists
    // Just verify it doesn't panic
    let _ = analyzer.is_available();
}

#[test]
fn test_opencode_discover_data_sources_no_panic() {
    let analyzer = OpenCodeAnalyzer::new();
    // Should return Ok even if no data exists
    let result = analyzer.discover_data_sources();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_opencode_parse_empty_sources() {
    let analyzer = OpenCodeAnalyzer::new();
    let result = analyzer.parse_conversations(vec![]).await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}
