use std::collections::HashMap;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    AnnotateAble, Implementation, ListResourcesResult, PaginatedRequestParam, ProtocolVersion,
    RawResource, ReadResourceRequestParam, ReadResourceResult, ResourceContents,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{
    ErrorData as McpError, Json, RoleServer, ServerHandler, ServiceExt, tool, tool_handler,
    tool_router,
};

use crate::types::{MultiAnalyzerStats, Stats};
use crate::{create_analyzer_registry, utils};

use super::types::*;

/// Resource URI constants
mod resource_uris {
    pub const DAILY_SUMMARY: &str = "splitrail://summary";
    pub const MODEL_BREAKDOWN: &str = "splitrail://models";
}

/// The Splitrail MCP Server
#[derive(Clone)]
pub struct SplitrailMcpServer {
    tool_router: ToolRouter<Self>,
}

impl SplitrailMcpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Load stats from all analyzers (reuses existing infrastructure)
    async fn load_stats(&self) -> Result<MultiAnalyzerStats, McpError> {
        let registry = create_analyzer_registry();
        registry
            .load_all_stats()
            .await
            .map_err(|e| McpError::internal_error(format!("Failed to load stats: {}", e), None))
    }

    /// Get daily stats for a specific analyzer or combined across all
    fn get_daily_stats_for_analyzer(
        stats: &MultiAnalyzerStats,
        analyzer: Option<&str>,
    ) -> std::collections::BTreeMap<String, crate::types::DailyStats> {
        if let Some(analyzer_name) = analyzer {
            // Find specific analyzer
            for analyzer_stats in &stats.analyzer_stats {
                if analyzer_stats
                    .analyzer_name
                    .eq_ignore_ascii_case(analyzer_name)
                {
                    return analyzer_stats.daily_stats.clone();
                }
            }
            std::collections::BTreeMap::new()
        } else {
            // Combine all messages and aggregate
            let all_messages: Vec<_> = stats
                .analyzer_stats
                .iter()
                .flat_map(|s| s.messages.iter().cloned())
                .collect();
            utils::aggregate_by_date(&all_messages)
        }
    }
}

#[tool_router]
impl SplitrailMcpServer {
    #[tool(
        name = "get_daily_stats",
        description = "Get daily usage statistics including messages, costs, tokens, and file operations. Can filter by date, analyzer, or limit to recent days."
    )]
    async fn get_daily_stats(
        &self,
        Parameters(req): Parameters<GetDailyStatsRequest>,
    ) -> Result<Json<DailyStatsResponse>, String> {
        let stats = self.load_stats().await.map_err(|e| e.to_string())?;
        let daily_stats = Self::get_daily_stats_for_analyzer(&stats, req.analyzer.as_deref());

        let mut results: Vec<DailySummary> = if let Some(date) = req.date {
            // Filter by specific date
            daily_stats
                .get(&date)
                .map(|ds| vec![DailySummary::from((date.as_str(), ds))])
                .unwrap_or_default()
        } else {
            // All dates
            daily_stats
                .iter()
                .map(|(date, ds)| DailySummary::from((date.as_str(), ds)))
                .collect()
        };

        // Sort by date descending (most recent first)
        results.sort_by(|a, b| b.date.cmp(&a.date));

        // Apply limit if specified
        if let Some(limit) = req.limit {
            results.truncate(limit);
        }

        Ok(Json(DailyStatsResponse { results }))
    }

    #[tool(
        name = "get_model_usage",
        description = "Get breakdown of AI model usage across conversations. Shows which models were used and how many messages each generated."
    )]
    async fn get_model_usage(
        &self,
        Parameters(req): Parameters<GetModelUsageRequest>,
    ) -> Result<Json<ModelUsageResponse>, String> {
        let stats = self.load_stats().await.map_err(|e| e.to_string())?;
        let daily_stats = Self::get_daily_stats_for_analyzer(&stats, req.analyzer.as_deref());

        let mut model_counts: HashMap<String, u32> = HashMap::new();

        if let Some(date) = req.date {
            if let Some(ds) = daily_stats.get(&date) {
                for (model, count) in &ds.models {
                    *model_counts.entry(model.clone()).or_insert(0) += count;
                }
            }
        } else {
            for ds in daily_stats.values() {
                for (model, count) in &ds.models {
                    *model_counts.entry(model.clone()).or_insert(0) += count;
                }
            }
        }

        let total_messages: u32 = model_counts.values().sum();
        let mut models: Vec<ModelUsageEntry> = model_counts
            .into_iter()
            .map(|(model, count)| ModelUsageEntry {
                model,
                message_count: count,
            })
            .collect();
        models.sort_by(|a, b| b.message_count.cmp(&a.message_count));

        Ok(Json(ModelUsageResponse {
            models,
            total_messages,
        }))
    }

    #[tool(
        name = "get_cost_breakdown",
        description = "Get cost breakdown over a date range. Shows total cost, daily costs, and average daily spending."
    )]
    async fn get_cost_breakdown(
        &self,
        Parameters(req): Parameters<GetCostBreakdownRequest>,
    ) -> Result<Json<CostBreakdownResponse>, String> {
        let stats = self.load_stats().await.map_err(|e| e.to_string())?;
        let daily_stats = Self::get_daily_stats_for_analyzer(&stats, req.analyzer.as_deref());

        let daily_costs: Vec<DailyCost> = daily_stats
            .iter()
            .filter(|(date, _)| {
                let after_start = req
                    .start_date
                    .as_ref()
                    .map(|s| date.as_str() >= s.as_str())
                    .unwrap_or(true);
                let before_end = req
                    .end_date
                    .as_ref()
                    .map(|e| date.as_str() <= e.as_str())
                    .unwrap_or(true);
                after_start && before_end
            })
            .map(|(date, ds)| DailyCost {
                date: date.clone(),
                cost: ds.stats.cost,
            })
            .collect();

        let total_cost: f64 = daily_costs.iter().map(|dc| dc.cost).sum();
        let average_daily_cost = if daily_costs.is_empty() {
            0.0
        } else {
            total_cost / daily_costs.len() as f64
        };

        Ok(Json(CostBreakdownResponse {
            total_cost,
            daily_costs,
            average_daily_cost,
        }))
    }

    #[tool(
        name = "get_file_operations",
        description = "Get file operation statistics including reads, writes, edits, and terminal commands."
    )]
    async fn get_file_operations(
        &self,
        Parameters(req): Parameters<GetFileOpsRequest>,
    ) -> Result<Json<FileOpsResponse>, String> {
        let stats = self.load_stats().await.map_err(|e| e.to_string())?;
        let daily_stats = Self::get_daily_stats_for_analyzer(&stats, req.analyzer.as_deref());

        let mut aggregated = Stats::default();

        if let Some(date) = req.date {
            if let Some(ds) = daily_stats.get(&date) {
                aggregated = ds.stats.clone();
            }
        } else {
            for ds in daily_stats.values() {
                aggregated.files_read += ds.stats.files_read;
                aggregated.files_edited += ds.stats.files_edited;
                aggregated.files_added += ds.stats.files_added;
                aggregated.files_deleted += ds.stats.files_deleted;
                aggregated.lines_read += ds.stats.lines_read;
                aggregated.lines_edited += ds.stats.lines_edited;
                aggregated.lines_added += ds.stats.lines_added;
                aggregated.lines_deleted += ds.stats.lines_deleted;
                aggregated.bytes_read += ds.stats.bytes_read;
                aggregated.bytes_edited += ds.stats.bytes_edited;
                aggregated.bytes_added += ds.stats.bytes_added;
                aggregated.bytes_deleted += ds.stats.bytes_deleted;
                aggregated.terminal_commands += ds.stats.terminal_commands;
                aggregated.file_searches += ds.stats.file_searches;
                aggregated.file_content_searches += ds.stats.file_content_searches;
            }
        }

        Ok(Json(FileOpsResponse::from(&aggregated)))
    }

    #[tool(
        name = "compare_tools",
        description = "Compare usage across different AI coding tools (Claude Code, Codex CLI, Gemini CLI, etc). Shows cost, messages, and activity for each."
    )]
    async fn compare_tools(
        &self,
        Parameters(req): Parameters<CompareToolsRequest>,
    ) -> Result<Json<ToolComparisonResponse>, String> {
        let stats = self.load_stats().await.map_err(|e| e.to_string())?;

        let tools: Vec<ToolSummary> = stats
            .analyzer_stats
            .iter()
            .map(|analyzer_stats| {
                let filtered_stats: Vec<_> = analyzer_stats
                    .daily_stats
                    .iter()
                    .filter(|(date, _)| {
                        let after_start = req
                            .start_date
                            .as_ref()
                            .map(|s| date.as_str() >= s.as_str())
                            .unwrap_or(true);
                        let before_end = req
                            .end_date
                            .as_ref()
                            .map(|e| date.as_str() <= e.as_str())
                            .unwrap_or(true);
                        after_start && before_end
                    })
                    .collect();

                let total_cost: f64 = filtered_stats.iter().map(|(_, ds)| ds.stats.cost).sum();
                let total_messages: u64 = filtered_stats
                    .iter()
                    .map(|(_, ds)| (ds.user_messages + ds.ai_messages) as u64)
                    .sum();
                let total_conversations: u64 = filtered_stats
                    .iter()
                    .map(|(_, ds)| ds.conversations as u64)
                    .sum();
                let total_tokens: u64 = filtered_stats
                    .iter()
                    .map(|(_, ds)| ds.stats.input_tokens + ds.stats.output_tokens)
                    .sum();
                let total_tool_calls: u32 = filtered_stats
                    .iter()
                    .map(|(_, ds)| ds.stats.tool_calls)
                    .sum();

                ToolSummary {
                    name: analyzer_stats.analyzer_name.clone(),
                    total_cost,
                    total_messages,
                    total_conversations,
                    total_tokens,
                    total_tool_calls,
                }
            })
            .collect();

        Ok(Json(ToolComparisonResponse { tools }))
    }

    #[tool(
        name = "list_analyzers",
        description = "List all available AI coding tool analyzers (e.g., Claude Code, Codex CLI, Gemini CLI, GitHub Copilot)."
    )]
    async fn list_analyzers(
        &self,
        Parameters(_req): Parameters<ListAnalyzersRequest>,
    ) -> Result<Json<AnalyzerListResponse>, String> {
        let registry = create_analyzer_registry();
        let analyzers: Vec<String> = registry
            .available_analyzers()
            .iter()
            .map(|a| a.display_name().to_string())
            .collect();

        Ok(Json(AnalyzerListResponse { analyzers }))
    }
}

#[tool_handler]
impl ServerHandler for SplitrailMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::LATEST,
            capabilities: ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
            server_info: Implementation {
                name: "splitrail".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                title: Some("Splitrail Analytics".to_string()),
                icons: None,
                website_url: Some("https://splitrail.dev".to_string()),
            },
            instructions: Some(
                "Splitrail MCP Server - Analytics for AI coding tools. \
                 Query daily stats, model usage, costs, file operations, and compare tools."
                    .to_string(),
            ),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParam>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult {
            resources: vec![
                RawResource::new(resource_uris::DAILY_SUMMARY, "Daily Summary".to_string())
                    .no_annotation(),
                RawResource::new(
                    resource_uris::MODEL_BREAKDOWN,
                    "Model Breakdown".to_string(),
                )
                .no_annotation(),
            ],
            next_cursor: None,
            meta: None,
        })
    }

    async fn read_resource(
        &self,
        ReadResourceRequestParam { uri }: ReadResourceRequestParam,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        match uri.as_str() {
            resource_uris::DAILY_SUMMARY => {
                let stats = self.load_stats().await?;
                let all_messages: Vec<_> = stats
                    .analyzer_stats
                    .iter()
                    .flat_map(|s| s.messages.iter().cloned())
                    .collect();
                let daily_stats = utils::aggregate_by_date(&all_messages);

                // Get most recent day's summary
                let summary = if let Some((date, ds)) = daily_stats.iter().next_back() {
                    format!(
                        "Date: {}\nMessages: {} user, {} AI\nConversations: {}\nCost: ${:.2}\nTokens: {} in, {} out",
                        date,
                        ds.user_messages,
                        ds.ai_messages,
                        ds.conversations,
                        ds.stats.cost,
                        ds.stats.input_tokens,
                        ds.stats.output_tokens
                    )
                } else {
                    "No data available".to_string()
                };

                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(summary, uri)],
                })
            }
            resource_uris::MODEL_BREAKDOWN => {
                let stats = self.load_stats().await?;
                let mut model_counts: HashMap<String, u32> = HashMap::new();

                for analyzer_stats in &stats.analyzer_stats {
                    for ds in analyzer_stats.daily_stats.values() {
                        for (model, count) in &ds.models {
                            *model_counts.entry(model.clone()).or_insert(0) += count;
                        }
                    }
                }

                let mut sorted: Vec<_> = model_counts.into_iter().collect();
                sorted.sort_by(|a, b| b.1.cmp(&a.1));

                let breakdown: String = sorted
                    .iter()
                    .map(|(model, count)| format!("{}: {} messages", model, count))
                    .collect::<Vec<_>>()
                    .join("\n");

                let content = if breakdown.is_empty() {
                    "No model usage data available".to_string()
                } else {
                    breakdown
                };

                Ok(ReadResourceResult {
                    contents: vec![ResourceContents::text(content, uri)],
                })
            }
            _ => Err(McpError::resource_not_found(
                "resource_not_found",
                Some(rmcp::serde_json::json!({ "uri": uri })),
            )),
        }
    }
}

/// Run the MCP server with stdio transport
pub async fn run_mcp_server() -> anyhow::Result<()> {
    use rmcp::transport::stdio;

    let server = SplitrailMcpServer::new();
    let service = server.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
