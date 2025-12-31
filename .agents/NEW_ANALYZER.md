# Adding a New Analyzer

Splitrail tracks token usage from AI coding agents. Each agent has its own "analyzer" that discovers and parses its data files.

## Quick Checklist

1. Add variant to `Application` enum in `src/types.rs`
2. Create analyzer in `src/analyzers/{agent_name}.rs`
3. Export in `src/analyzers/mod.rs`
4. Register in `src/main.rs`
5. Add tests in `src/analyzers/tests/{agent_name}.rs`
6. Update README.md

## Step 1: Add Application Variant

In `src/types.rs`, add to the `Application` enum:

```rust
pub enum Application {
    // ... existing variants
    YourAgent,
}
```

## Step 2: Create the Analyzer

Create `src/analyzers/{agent_name}.rs`. Key components:

### Analyzer Struct

```rust
pub struct YourAgentAnalyzer;

impl YourAgentAnalyzer {
    pub fn new() -> Self { Self }

    /// Returns the root directory for this agent's data.
    fn data_dir() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".your-agent").join("data"))
    }
}
```

### Implement the Analyzer Trait

```rust
#[async_trait]
impl Analyzer for YourAgentAnalyzer {
    fn display_name(&self) -> &'static str {
        "Your Agent"  // Shown in the TUI
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        // Glob patterns for finding data files
        vec![format!("{}/.your-agent/data/*.json", home_dir)]
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        // Use jwalk for fast parallel directory walking
        // Return paths to data files
    }

    async fn parse_conversations(&self, sources: Vec<DataSource>) -> Result<Vec<ConversationMessage>> {
        // Parse each data file into ConversationMessage structs
        // Use rayon's into_par_iter() for parallel processing
        // Use simd_json for fast JSON parsing
    }

    async fn get_stats(&self) -> Result<AgenticCodingToolStats> {
        let sources = self.discover_data_sources()?;
        let messages = self.parse_conversations(sources).await?;
        let daily_stats = crate::utils::aggregate_by_date(&messages);
        // ... standard aggregation
    }

    fn is_available(&self) -> bool {
        self.discover_data_sources().is_ok_and(|s| !s.is_empty())
    }

    fn get_watch_directories(&self) -> Vec<PathBuf> {
        // Return root directories for file watching
        Self::data_dir().filter(|d| d.is_dir()).into_iter().collect()
    }
}
```

### ConversationMessage Fields

Each message needs:
- `application`: Your `Application::YourAgent` variant
- `date`: Timestamp as `DateTime<Utc>`
- `project_hash`: Hash of project/workspace path
- `conversation_hash`: Hash of session/conversation ID
- `local_hash`: Optional unique message ID within the agent
- `global_hash`: Unique ID across all Splitrail data (for deduplication on upload)
- `model`: Model name (e.g., "claude-sonnet-4-5")
- `stats`: Token counts, costs, tool calls (see `Stats` struct)
- `role`: `MessageRole::User` or `MessageRole::Assistant`
- `session_name`: Human-readable session title

### Stats Extraction

Populate `Stats` with:
- `input_tokens`, `output_tokens`, `cache_*_tokens`
- `cost`: Use `models::calculate_total_cost()` if agent doesn't provide cost
- `tool_calls`, `files_read`, `files_edited`, etc.

## Step 3: Export the Analyzer

In `src/analyzers/mod.rs`:

```rust
pub mod your_agent;
pub use your_agent::YourAgentAnalyzer;
```

## Step 4: Register the Analyzer

In `src/main.rs`:

```rust
use analyzers::YourAgentAnalyzer;
// ...
registry.register(YourAgentAnalyzer::new());
```

## Step 5: Add Tests

Create `src/analyzers/tests/{agent_name}.rs`:

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

Export in `src/analyzers/tests/mod.rs`:
```rust
mod your_agent;
```

## Key Patterns

### VS Code Extensions

For Cline-like VS Code extensions, use the helper:
```rust
use crate::analyzer::discover_vscode_extension_sources;

fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
    discover_vscode_extension_sources(
        "publisher.extension-id",  // e.g., "saoudrizwan.claude-dev"
        "ui_messages.json",        // target filename
        true,                      // return parent directory
    )
}
```

### CLI Tools with JSONL

For CLI tools storing JSONL files (like Claude Code, Pi Agent):
```rust
// Parse JSONL line by line with simd_json
for line in buffer.split(|&b| b == b'\n') {
    let mut line_buf = line.to_vec();
    let entry = simd_json::from_slice::<YourEntry>(&mut line_buf)?;
    // ...
}
```

### Cross-Platform Paths

Use `dirs::home_dir()` for home directory:
- Linux: `~/.config/...` or `~/.local/share/...`
- macOS: `~/Library/Application Support/...`
- Windows: `%APPDATA%\...`

## Model Pricing

If the agent doesn't provide cost data, add model pricing to `src/models.rs`:
1. Add to `MODEL_INDEX` with `ModelInfo` (pricing structure, caching)
2. Add aliases to `MODEL_ALIASES` (date suffixes, etc.)

## Example Agents

- **Simple JSONL CLI**: `pi_agent.rs` - Good starting template
- **VS Code extension**: `cline.rs`, `roo_code.rs`
- **Complex with dedup**: `claude_code.rs`
- **External data dirs**: `opencode.rs`
