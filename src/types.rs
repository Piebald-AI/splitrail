use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::utils::ModelAbbreviations;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Application {
    ClaudeCode,
    GeminiCLI,
    CodexCLI,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConversationMessage {
    #[serde(rename_all = "camelCase")]
    #[serde(rename = "AI")]
    AI {
        application: Application,
        model: String,
        timestamp: String,
        #[serde(skip)]
        hash: Option<String>,
        conversation_file: String,
        file_operations: FileOperationStats,
        general_stats: GeneralStats,
        #[serde(skip_serializing_if = "Option::is_none")]
        todo_stats: Option<TodoStats>,
        composition_stats: CompositionStats,
        #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
        analyzer_specific: std::collections::HashMap<String, serde_json::Value>,
    },
    #[serde(rename = "User")]
    User {
        timestamp: String,
        application: Application,
        conversation_file: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        todo_stats: Option<TodoStats>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub todo_stats: Option<TodoStats>,
    pub composition_stats: CompositionStats,
    pub max_flow_length_seconds: u64, // Longest autonomous AI operation in seconds
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cache_creation_input_token_cost: f64,
    pub cache_read_input_token_cost: f64,
    pub model_rules: crate::models::ModelSpecificRules,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationStats {
    pub file_types: BTreeMap<String, u64>, // grouped by category
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoStats {
    pub todos_created: u64,
    pub todos_completed: u64,
    pub todos_in_progress: u64,
    pub todo_writes: u64,
    pub todo_reads: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompositionStats {
    pub code_lines: u64,
    pub docs_lines: u64,
    pub data_lines: u64,
    pub media_lines: u64,
    pub config_lines: u64,
    pub other_lines: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneralStats {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_tokens: u64,
    pub cache_read_tokens: u64,
    pub cached_tokens: u64,
    pub cost: f64,
    pub tool_calls: u32,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultiAnalyzerStats {
    pub analyzer_stats: Vec<AgenticCodingToolStats>,
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
