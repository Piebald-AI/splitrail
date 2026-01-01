use crate::analyzer::Analyzer;
use crate::analyzers::qwen_code::QwenCodeAnalyzer;

#[test]
fn test_qwen_code_analyzer_creation() {
    let analyzer = QwenCodeAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Qwen Code");
}

#[test]
fn test_qwen_code_is_available() {
    let analyzer = QwenCodeAnalyzer::new();
    // is_available depends on whether Qwen Code extension data exists
    // Just verify it doesn't panic
    let _ = analyzer.is_available();
}

#[test]
fn test_qwen_code_discover_data_sources_no_panic() {
    let analyzer = QwenCodeAnalyzer::new();
    // Should return Ok even if no data exists
    let result = analyzer.discover_data_sources();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_qwen_code_get_stats_empty_sources() {
    let analyzer = QwenCodeAnalyzer::new();
    let result = analyzer.get_stats_with_sources(vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().messages.is_empty());
}
