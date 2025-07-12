use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::utils::ModelAbbreviations;

#[derive(Debug, Clone)]
pub enum ConversationMessage {
    AI {
        input_tokens: u64,
        output_tokens: u64,
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
        cost: f64,
        model: String,
        timestamp: String,
        #[allow(dead_code)]
        message_id: Option<String>,
        #[allow(dead_code)]
        request_id: Option<String>,
        #[allow(dead_code)]
        has_cost_usd: bool,
        tool_calls: u32,
        #[allow(dead_code)]
        entry_type: Option<String>,
        hash: Option<String>,
        #[allow(dead_code)]
        is_user_message: bool,
        conversation_file: String,
        file_operations: FileOperationStats,
        todo_stats: TodoStats,
    },
    User {
        timestamp: String,
        conversation_file: String,
        todo_stats: TodoStats,
    },
}

#[derive(Debug, Clone, Default)]
pub struct DailyStats {
    #[allow(dead_code)]
    pub date: String,
    pub cost: f64,
    pub cached_tokens: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub user_messages: u32,
    pub ai_messages: u32,
    pub tool_calls: u32,
    pub conversations: u32,
    pub models: BTreeMap<String, u32>,
    pub file_operations: FileOperationStats,
    pub todo_stats: TodoStats,
    pub max_flow_length_seconds: u64, // Longest autonomous AI operation in seconds
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_creation_input_token_cost: f64,
    pub cache_read_input_token_cost: f64,
}

#[derive(Debug, Clone, Default)]
pub struct FileOperationStats {
    pub files_read: u32,
    pub files_edited: u32,
    pub files_written: u32,
    pub file_types: BTreeMap<String, u32>, // grouped by category
    pub bash_commands: u32,
    pub glob_searches: u32,
    pub grep_searches: u32,
    pub lines_read: u64,
    pub lines_edited: u64,
    pub lines_written: u64,
    pub bytes_read: u64,
    pub bytes_edited: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Default)]
pub struct TodoStats {
    pub todos_created: u32,
    pub todos_completed: u32,
    pub todos_in_progress: u32,
    pub todo_writes: u32,
    pub todo_reads: u32,
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

    pub fn as_str(&self) -> &'static str {
        match self {
            FileCategory::SourceCode => "source_code",
            FileCategory::Data => "data",
            FileCategory::Documentation => "documentation",
            FileCategory::Media => "media",
            FileCategory::Config => "config",
            FileCategory::Other => "other",
        }
    }
}

pub struct AgenticCodingToolStats {
    pub daily_stats: BTreeMap<String, DailyStats>,
    pub num_conversations: u64,
    pub model_abbrs: ModelAbbreviations,
}
