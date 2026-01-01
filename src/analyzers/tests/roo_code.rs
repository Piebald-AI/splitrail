use crate::analyzer::Analyzer;
use crate::analyzers::roo_code::RooCodeAnalyzer;

#[test]
fn test_roo_code_analyzer_creation() {
    let analyzer = RooCodeAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Roo Code");
}

#[test]
fn test_roo_code_is_available() {
    let analyzer = RooCodeAnalyzer::new();
    // is_available depends on whether Roo Code extension data exists
    // Just verify it doesn't panic
    let _ = analyzer.is_available();
}

#[test]
fn test_roo_code_discover_data_sources_no_panic() {
    let analyzer = RooCodeAnalyzer::new();
    // Should return Ok even if no data exists
    let result = analyzer.discover_data_sources();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_roo_code_get_stats_empty_sources() {
    let analyzer = RooCodeAnalyzer::new();
    let result = analyzer.get_stats_with_sources(vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().messages.is_empty());
}
