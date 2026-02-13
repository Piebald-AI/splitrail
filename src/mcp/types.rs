use schemars::JsonSchema;
use schemars::transform::RecursiveTransform;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::types::DailyStats;

/// Strips non-standard numeric `format` annotations from JSON Schemas.
///
/// The `schemars` crate emits format values like `"uint64"`, `"int32"`, and `"double"` for Rust
/// numeric types. These are not defined by the JSON Schema specification and cause noisy warnings
/// in strict validators such as `ajv` (used by OpenCode and other MCP clients).
///
/// See: <https://github.com/Piebald-AI/splitrail/issues/113>
fn strip_non_standard_format(schema: &mut schemars::Schema) {
    let dominated = schema
        .get("format")
        .and_then(|v| v.as_str())
        .is_some_and(|f| {
            matches!(
                f,
                "uint8"
                    | "int8"
                    | "uint16"
                    | "int16"
                    | "uint32"
                    | "int32"
                    | "uint64"
                    | "int64"
                    | "uint"
                    | "int"
                    | "float"
                    | "double"
            )
        });
    if dominated {
        schema.remove("format");
    }
}

// ============================================================================
// Request Types
// ============================================================================

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[schemars(transform = RecursiveTransform(strip_non_standard_format))]
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
#[schemars(transform = RecursiveTransform(strip_non_standard_format))]
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

/// File operation stats computed on-demand from raw messages.
/// Used to supplement DailyStats (which only contains TUI-relevant fields).
#[derive(Debug, Clone, Default)]
pub struct DateFileOps {
    pub files_read: u64,
    pub files_edited: u64,
    pub files_added: u64,
    pub terminal_commands: u64,
}

impl DailySummary {
    /// Create a DailySummary from DailyStats and file operation stats.
    /// File ops are computed separately from raw messages since DailyStats
    /// only contains TUI-relevant fields.
    pub fn new(date: &str, ds: &DailyStats, file_ops: &DateFileOps) -> Self {
        Self {
            date: date.to_string(),
            user_messages: ds.user_messages,
            ai_messages: ds.ai_messages,
            conversations: ds.conversations,
            total_cost: ds.stats.cost(),
            input_tokens: ds.stats.input_tokens as u64,
            output_tokens: ds.stats.output_tokens as u64,
            cache_read_tokens: ds.stats.cached_tokens as u64,
            tool_calls: ds.stats.tool_calls,
            files_read: file_ops.files_read,
            files_edited: file_ops.files_edited,
            files_added: file_ops.files_added,
            terminal_commands: file_ops.terminal_commands,
            models: ds.models.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(transform = RecursiveTransform(strip_non_standard_format))]
pub struct ModelUsageEntry {
    pub model: String,
    pub message_count: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(transform = RecursiveTransform(strip_non_standard_format))]
pub struct ModelUsageResponse {
    pub models: Vec<ModelUsageEntry>,
    pub total_messages: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(transform = RecursiveTransform(strip_non_standard_format))]
pub struct DailyCost {
    pub date: String,
    pub cost: f64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(transform = RecursiveTransform(strip_non_standard_format))]
pub struct CostBreakdownResponse {
    pub total_cost: f64,
    pub daily_costs: Vec<DailyCost>,
    pub average_daily_cost: f64,
}

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[schemars(transform = RecursiveTransform(strip_non_standard_format))]
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

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(transform = RecursiveTransform(strip_non_standard_format))]
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

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::serde_json;
    use schemars::schema_for;
    use schemars::transform::Transform;

    /// Formats that schemars emits for Rust numeric types but that are not
    /// part of the JSON Schema specification.
    const NON_STANDARD_FORMATS: &[&str] = &[
        "uint8", "int8", "uint16", "int16", "uint32", "int32", "uint64", "int64", "uint", "int",
        "float", "double",
    ];

    /// Recursively check that no value in the JSON tree equals any of the
    /// non-standard format strings.
    fn assert_no_non_standard_formats(value: &serde_json::Value, path: &str) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(fmt) = map.get("format").and_then(|v| v.as_str()) {
                    assert!(
                        !NON_STANDARD_FORMATS.contains(&fmt),
                        "found non-standard format \"{fmt}\" at {path}/format"
                    );
                }
                for (key, val) in map {
                    assert_no_non_standard_formats(val, &format!("{path}/{key}"));
                }
            }
            serde_json::Value::Array(arr) => {
                for (i, val) in arr.iter().enumerate() {
                    assert_no_non_standard_formats(val, &format!("{path}[{i}]"));
                }
            }
            _ => {}
        }
    }

    #[test]
    fn mcp_schemas_contain_no_non_standard_formats() {
        // Generate schemas for every MCP type that has numeric fields and
        // verify the transform successfully stripped the non-standard format
        // annotations.
        let schemas: Vec<(&str, schemars::Schema)> = vec![
            ("GetDailyStatsRequest", schema_for!(GetDailyStatsRequest)),
            ("DailySummary", schema_for!(DailySummary)),
            ("ModelUsageEntry", schema_for!(ModelUsageEntry)),
            ("ModelUsageResponse", schema_for!(ModelUsageResponse)),
            ("DailyCost", schema_for!(DailyCost)),
            ("CostBreakdownResponse", schema_for!(CostBreakdownResponse)),
            ("FileOpsResponse", schema_for!(FileOpsResponse)),
            ("ToolSummary", schema_for!(ToolSummary)),
        ];

        for (name, schema) in &schemas {
            let value = serde_json::to_value(schema).expect("schema should serialize");
            assert_no_non_standard_formats(&value, &format!("#/{name}"));
        }
    }

    #[test]
    fn strip_non_standard_format_is_selective() {
        // Verify the transform only strips non-standard formats and leaves
        // standard ones (like "date-time") untouched.
        let mut schema = schemars::json_schema!({
            "type": "string",
            "format": "date-time"
        });

        let mut transform = RecursiveTransform(super::strip_non_standard_format);
        transform.transform(&mut schema);

        assert_eq!(
            schema.get("format").and_then(|v| v.as_str()),
            Some("date-time"),
            "standard formats must not be stripped"
        );
    }
}
