use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Application {
    ClaudeCode,
    GeminiCli,
    CodexCli,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[derive(PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    pub application: Application,
    pub timestamp: String,
    pub hash: String,
    pub project_hash: String,
    pub model: Option<String>, // None for user messages
    pub stats: Stats,
    pub role: MessageRole,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DailyStats {
    #[allow(dead_code)]
    pub date: String,
    pub user_messages: u32,
    pub ai_messages: u32,
    pub conversations: u32,
    pub models: BTreeMap<String, u32>,
    pub stats: Stats,
    pub max_flow_length_seconds: u64, // Longest autonomous AI operation in seconds
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    // Token and cost stats
    pub input_tokens: u64,
    pub output_tokens: u64,
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

    // Composition stats
    pub code_lines: u64,
    pub docs_lines: u64,
    pub data_lines: u64,
    pub media_lines: u64,
    pub config_lines: u64,
    pub other_lines: u64,
}

#[derive(Debug, Clone, Copy)]
pub enum FileCategory {
    SourceCode,
    Data,
    Documentation,
    Media,
    Config,
    Other,
}

impl FileCategory {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "java" | "cpp" | "c" | "h" | "hpp"
            | "cs" | "go" | "php" | "rb" | "swift" | "kt" | "scala" | "clj" | "hs" | "ml"
            | "fs" | "elm" | "dart" | "lua" | "r" | "jl" | "nim" | "zig" | "v" | "odin" => {
                FileCategory::SourceCode
            }
            "json" | "xml" | "yaml" | "yml" | "toml" | "ini" | "csv" | "tsv" | "sql" | "db"
            | "sqlite" | "sqlite3" => FileCategory::Data,
            "md" | "txt" | "rst" | "adoc" | "tex" | "rtf" | "doc" | "docx" | "pdf" | "html"
            | "htm" => FileCategory::Documentation,
            "png" | "jpg" | "jpeg" | "gif" | "bmp" | "svg" | "ico" | "webp" | "tiff" | "mp3"
            | "wav" | "mp4" | "avi" | "mkv" | "mov" | "wmv" | "flv" | "webm" => FileCategory::Media,
            "config" | "conf" | "cfg" | "env" | "properties" | "plist" | "reg" | "desktop"
            | "service" => FileCategory::Config,
            _ => FileCategory::Other,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticCodingToolStats {
    pub daily_stats: BTreeMap<String, DailyStats>,
    pub num_conversations: u64,
    pub messages: Vec<ConversationMessage>,
    pub analyzer_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAnalyzerStats {
    pub analyzer_stats: Vec<AgenticCodingToolStats>,
}

#[derive(Debug, Deserialize)]
pub struct UploadResponse {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}
