use crate::analyzer::Analyzer;
use crate::analyzers::cline::ClineAnalyzer;

#[test]
fn test_cline_analyzer_creation() {
    let analyzer = ClineAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Cline");
}

#[test]
fn test_cline_is_available() {
    let analyzer = ClineAnalyzer::new();
    // is_available depends on whether Cline extension data exists
    // Just verify it doesn't panic
    let _ = analyzer.is_available();
}

#[test]
fn test_cline_discover_data_sources_no_panic() {
    let analyzer = ClineAnalyzer::new();
    // Should return Ok even if no data exists
    let result = analyzer.discover_data_sources();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_cline_get_stats_empty_sources() {
    let analyzer = ClineAnalyzer::new();
    let result = analyzer.get_stats_with_sources(vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().messages.is_empty());
}
