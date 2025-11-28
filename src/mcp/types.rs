use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::types::{DailyStats, Stats};

// ============================================================================
// Request Types
// ============================================================================

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetDailyStatsRequest {
    /// Filter by specific date (YYYY-MM-DD format). If omitted, returns all dates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,

    /// Filter by analyzer name (e.g., "Claude Code", "Gemini CLI"). If omitted, returns combined stats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyzer: Option<String>,

    /// Number of most recent days to return. If omitted, returns all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetModelUsageRequest {
    /// Filter by specific date (YYYY-MM-DD format). If omitted, returns all-time usage.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,

    /// Filter by analyzer name. If omitted, returns combined usage across all tools.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyzer: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetCostBreakdownRequest {
    /// Start date for cost breakdown (YYYY-MM-DD). If omitted, uses earliest available date.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,

    /// End date for cost breakdown (YYYY-MM-DD). If omitted, uses today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_date: Option<String>,

    /// Filter by analyzer name. If omitted, returns combined costs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyzer: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct GetFileOpsRequest {
    /// Filter by specific date (YYYY-MM-DD format). If omitted, returns all-time stats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,

    /// Filter by analyzer name. If omitted, returns combined file operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub analyzer: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct CompareToolsRequest {
    /// Start date for comparison (YYYY-MM-DD). If omitted, uses all available data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,

    /// End date for comparison (YYYY-MM-DD). If omitted, uses today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_date: Option<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct ListAnalyzersRequest {}

// ============================================================================
// Response Types
// ============================================================================

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DailySummary {
    pub date: String,
    pub user_messages: u32,
    pub ai_messages: u32,
    pub conversations: u32,
    pub total_cost: f64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub tool_calls: u32,
    pub files_read: u64,
    pub files_edited: u64,
    pub files_added: u64,
    pub terminal_commands: u64,
    pub models: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DailyStatsResponse {
    pub results: Vec<DailySummary>,
}

impl From<(&str, &DailyStats)> for DailySummary {
    fn from((date, ds): (&str, &DailyStats)) -> Self {
        Self {
            date: date.to_string(),
            user_messages: ds.user_messages,
            ai_messages: ds.ai_messages,
            conversations: ds.conversations,
            total_cost: ds.stats.cost,
            input_tokens: ds.stats.input_tokens,
            output_tokens: ds.stats.output_tokens,
            cache_read_tokens: ds.stats.cache_read_tokens,
            tool_calls: ds.stats.tool_calls,
            files_read: ds.stats.files_read,
            files_edited: ds.stats.files_edited,
            files_added: ds.stats.files_added,
            terminal_commands: ds.stats.terminal_commands,
            models: ds.models.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModelUsageEntry {
    pub model: String,
    pub message_count: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModelUsageResponse {
    pub models: Vec<ModelUsageEntry>,
    pub total_messages: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct DailyCost {
    pub date: String,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct CostBreakdownResponse {
    pub total_cost: f64,
    pub daily_costs: Vec<DailyCost>,
    pub average_daily_cost: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct FileOpsResponse {
    pub files_read: u64,
    pub files_edited: u64,
    pub files_added: u64,
    pub files_deleted: u64,
    pub lines_read: u64,
    pub lines_edited: u64,
    pub lines_added: u64,
    pub lines_deleted: u64,
    pub bytes_read: u64,
    pub bytes_edited: u64,
    pub bytes_added: u64,
    pub bytes_deleted: u64,
    pub terminal_commands: u64,
    pub file_searches: u64,
    pub file_content_searches: u64,
}

impl From<&Stats> for FileOpsResponse {
    fn from(s: &Stats) -> Self {
        Self {
            files_read: s.files_read,
            files_edited: s.files_edited,
            files_added: s.files_added,
            files_deleted: s.files_deleted,
            lines_read: s.lines_read,
            lines_edited: s.lines_edited,
            lines_added: s.lines_added,
            lines_deleted: s.lines_deleted,
            bytes_read: s.bytes_read,
            bytes_edited: s.bytes_edited,
            bytes_added: s.bytes_added,
            bytes_deleted: s.bytes_deleted,
            terminal_commands: s.terminal_commands,
            file_searches: s.file_searches,
            file_content_searches: s.file_content_searches,
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ToolSummary {
    pub name: String,
    pub total_cost: f64,
    pub total_messages: u64,
    pub total_conversations: u64,
    pub total_tokens: u64,
    pub total_tool_calls: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ToolComparisonResponse {
    pub tools: Vec<ToolSummary>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct AnalyzerListResponse {
    pub analyzers: Vec<String>,
}
