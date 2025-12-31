# Testing Strategy

## Test Organization

- **Unit tests**: Inline with source using `#[cfg(test)] mod tests`
- **Integration tests**: `src/analyzers/tests/` for analyzer-specific parsing tests

## Test Data

Most analyzers use real-world JSON fixtures in test modules to verify parsing logic. See `src/analyzers/tests/source_data/` for examples.

## Adding Tests for a New Analyzer

1. Create `src/analyzers/tests/{agent_name}.rs`
2. Add module to `src/analyzers/tests/mod.rs`

Example test structure:

```rust
use crate::analyzer::Analyzer;
use crate::analyzers::your_agent::YourAgentAnalyzer;

#[test]
fn test_analyzer_creation() {
    let analyzer = YourAgentAnalyzer::new();
    assert_eq!(analyzer.display_name(), "Your Agent");
}

#[test]
fn test_discover_no_panic() {
    let analyzer = YourAgentAnalyzer::new();
    assert!(analyzer.discover_data_sources().is_ok());
}

#[tokio::test]
async fn test_parse_empty() {
    let analyzer = YourAgentAnalyzer::new();
    let result = analyzer.parse_conversations(vec![]).await;
    assert!(result.is_ok());
}
```

## Running Tests

```bash
cargo test --quiet
```

For a specific analyzer:
```bash
cargo test analyzers::tests::claude_code
```
