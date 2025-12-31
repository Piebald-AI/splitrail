# Key Types

Core data structures in `src/types.rs`.

## ConversationMessage

The normalized message format across all analyzers.

```rust
pub struct ConversationMessage {
    pub application: Application,       // Which AI tool (ClaudeCode, Copilot, etc.)
    pub date: DateTime<Utc>,            // Message timestamp
    pub project_hash: String,           // Hash of project/workspace path
    pub conversation_hash: String,      // Hash of session/conversation ID
    pub local_hash: Option<String>,     // Unique message ID within the agent
    pub global_hash: String,            // Unique ID across all Splitrail data (for dedup)
    pub model: Option<String>,          // Model name (None for user messages)
    pub stats: Stats,                   // Token counts, costs, tool calls
    pub role: MessageRole,              // User or Assistant
    pub uuid: Option<String>,           // Unique identifier if available
    pub session_name: Option<String>,   // Human-readable session title
}
```

### Hashing Strategy

- `local_hash`: Used for deduplication within a single analyzer
- `global_hash`: Used for deduplication on upload to Splitrail Cloud

## Stats

Comprehensive usage metrics for a single message.

```rust
pub struct Stats {
    // Token and cost stats
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub tool_calls: u32,

    // File operation stats
    pub terminal_commands: u64,
    pub file_searches: u64,
    pub file_content_searches: u64,
    pub files_read: u64,
    pub files_added: u64,
    pub files_edited: u64,
    pub files_deleted: u64,
    pub lines_read: u64,
    pub lines_added: u64,
    pub lines_edited: u64,
    pub lines_deleted: u64,
    pub bytes_read: u64,
    pub bytes_added: u64,
    pub bytes_edited: u64,
    pub bytes_deleted: u64,

    // Todo stats
    pub todos_created: u64,
    pub todos_completed: u64,
    pub todos_in_progress: u64,
    pub todo_writes: u64,
    pub todo_reads: u64,

    // Composition stats (lines by file type)
    pub code_lines: u64,
    pub docs_lines: u64,
    pub data_lines: u64,
    pub media_lines: u64,
    pub config_lines: u64,
    pub other_lines: u64,
}
```

## DailyStats

Pre-aggregated stats per date.

```rust
pub struct DailyStats {
    pub date: String,
    pub user_messages: u32,
    pub ai_messages: u32,
    pub conversations: u32,
    pub models: BTreeMap<String, u32>,
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
