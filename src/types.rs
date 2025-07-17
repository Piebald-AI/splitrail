use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::analyzer::CachingInfo;
use crate::utils::ModelAbbreviations;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConversationMessage {
    #[serde(rename_all = "camelCase")]
    #[serde(rename = "AI")]
    AI {
        input_tokens: u64,
        output_tokens: u64,
        
        // Legacy fields for backward compatibility
        #[serde(default)]
        cache_creation_tokens: u64,
        #[serde(default)]
        cache_read_tokens: u64,
        
        // New flexible caching structure
        #[serde(skip_serializing_if = "Option::is_none")]
        caching_info: Option<CachingInfo>,
        
        cost: f64,
        model: String,
        timestamp: String,
        tool_calls: u32,
        hash: Option<String>,
        conversation_file: String,
        file_operations: FileOperationStats,
        todo_stats: TodoStats,
        
        // Tool-specific data
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        analyzer_specific: std::collections::HashMap<String, serde_json::Value>,
    },
    #[serde(rename = "User")]
    User {
        timestamp: String,
        conversation_file: String,
        todo_stats: TodoStats,
        
        // Tool-specific data
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        analyzer_specific: std::collections::HashMap<String, serde_json::Value>,
    },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationStats {
    pub files_read: u32,
    pub files_edited: u32,
    pub files_written: u32,
    pub file_types: BTreeMap<String, u32>, // grouped by category
    pub terminal_commands: u32,
    pub glob_searches: u32,
    pub grep_searches: u32,
    pub lines_read: u64,
    pub lines_edited: u64,
    pub lines_written: u64,
    pub bytes_read: u64,
    pub bytes_edited: u64,
    pub bytes_written: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgenticCodingToolStats {
    pub daily_stats: BTreeMap<String, DailyStats>,
    pub num_conversations: u64,
    pub model_abbrs: ModelAbbreviations,
    pub messages: Vec<ConversationMessage>,
    pub analyzer_name: String,
}

#[derive(Debug, Serialize)]
pub struct UploadStatsRequest {
    pub date: String,
    pub stats: WebappStats,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebappStats {
    pub hash: String,
    pub message: ConversationMessage,
}

#[derive(Debug, Serialize)]
pub struct ProjectData {
    pub percentage: f64,
    pub lines: u64,
}

#[derive(Debug, Serialize)]
pub struct LanguageData {
    pub lines: u64,
    pub files: u64,
}

#[derive(Debug, Deserialize)]
pub struct UploadResponse {
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
}
