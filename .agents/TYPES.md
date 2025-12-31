# Key Types

Core data structures in `src/types.rs`.

## ConversationMessage

The normalized message format across all analyzers.

```rust
pub struct ConversationMessage {
    pub application: Application,      // Which AI tool (ClaudeCode, Copilot, etc.)
    pub date: DateTime<Utc>,           // Message timestamp
    pub project_hash: String,          // Hash of project/workspace path
    pub conversation_hash: String,     // Hash of session/conversation ID
    pub local_hash: Option<String>,    // Unique message ID within the agent
    pub global_hash: String,           // Unique ID across all Splitrail data (for dedup)
    pub model: String,                 // Model name (e.g., "claude-sonnet-4-5")
    pub stats: Stats,                  // Token counts, costs, tool calls
    pub role: MessageRole,             // User or Assistant
    pub session_name: String,          // Human-readable session title
}
```

### Hashing Strategy

- `local_hash`: Used for deduplication within a single analyzer
- `global_hash`: Used for deduplication on upload to Splitrail Cloud

## Stats

Comprehensive usage metrics for a single message.

```rust
pub struct Stats {
    // Token counts
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    
    // Cost
    pub cost: f64,
    
    // Tool usage
    pub tool_calls: u32,
    pub files_read: u32,
    pub files_edited: u32,
    pub files_deleted: u32,
    
    // Detailed operations
    pub lines_read: u64,
    pub lines_edited: u64,
    pub bytes_read: u64,
    pub bytes_edited: u64,
    
    // File categorization
    pub code_files: u32,
    pub doc_files: u32,
    pub data_files: u32,
    pub media_files: u32,
    pub config_files: u32,
    
    // Todo tracking
    pub todos_created: u32,
    pub todos_completed: u32,
    pub todos_in_progress: u32,
}
```

## DailyStats

Pre-aggregated stats per date.

```rust
pub struct DailyStats {
    pub message_count: u32,
    pub conversation_count: u32,
    pub model_breakdown: HashMap<String, u32>,
    pub stats: Stats,  // Embedded aggregate stats
}
```

## Application Enum

Identifies which AI coding tool a message came from:

```rust
pub enum Application {
    ClaudeCode,
    Copilot,
    Cline,
    RooCode,
    KiloCode,
    CodexCli,
    GeminiCli,
    QwenCode,
    OpenCode,
    PiAgent,
    Piebald,
}
```

## Aggregation

Use `crate::utils::aggregate_by_date()` to group messages into `DailyStats`:

```rust
let daily_stats: BTreeMap<String, DailyStats> = utils::aggregate_by_date(&messages);
```
