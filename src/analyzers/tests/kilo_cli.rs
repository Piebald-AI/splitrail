use crate::analyzer::Analyzer;
use crate::analyzers::kilo_cli::KiloCliAnalyzer;

#[test]
fn test_kilo_cli_analyzer_creation() {
    let analyzer = KiloCliAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Kilo CLI");
}

#[test]
fn test_kilo_cli_is_available() {
    let analyzer = KiloCliAnalyzer::new();
    // is_available depends on whether Kilo CLI data exists
    // Just verify it doesn't panic
    let _ = analyzer.is_available();
}

#[test]
fn test_kilo_cli_discover_data_sources_no_panic() {
    let analyzer = KiloCliAnalyzer::new();
    // Should return Ok even if no data exists
    let result = analyzer.discover_data_sources();
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_kilo_cli_get_stats_empty_sources() {
    let analyzer = KiloCliAnalyzer::new();
    let result = analyzer.get_stats_with_sources(vec![]);
    assert!(result.is_ok());
    assert!(result.unwrap().messages.is_empty());
}
