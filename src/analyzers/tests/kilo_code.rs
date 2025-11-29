use crate::analyzer::Analyzer;
use crate::analyzers::kilo_code::KiloCodeAnalyzer;

#[test]
fn test_kilo_code_analyzer_creation() {
    let analyzer = KiloCodeAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Kilo Code");
}

#[test]
fn test_kilo_code_is_available() {
    let analyzer = KiloCodeAnalyzer::new();
    // is_available depends on whether Kilo Code extension data exists
    // Just verify it doesn't panic
    let _ = analyzer.is_available();
}

#[test]
fn test_kilo_code_discover_data_sources_no_panic() {
    let analyzer = KiloCodeAnalyzer::new();
    // Should return Ok even if no data exists
    let result = analyzer.discover_data_sources();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_kilo_code_parse_empty_sources() {
    let analyzer = KiloCodeAnalyzer::new();
    let result = analyzer.parse_conversations(vec![]).await;
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}
