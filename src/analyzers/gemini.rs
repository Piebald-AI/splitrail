use crate::analyzer::{Analyzer, DataSource};
use crate::models::MODEL_PRICING;
use crate::types::{
    AgenticCodingToolStats, Application, ConversationMessage, DailyStats, FileCategory, MessageRole, Stats
};
use crate::utils::ModelAbbreviations;
use anyhow::Result;
use async_trait::async_trait;
use chrono::DateTime;
use glob::glob;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

pub struct GeminiAnalyzer;

impl GeminiAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

// Gemini-specific data structures following the plan's simplified flat approach
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiSession {
    session_id: String,
    project_hash: String,
    start_time: String,
    last_updated: String,
    messages: Vec<GeminiMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
enum GeminiMessage {
    #[serde(rename = "user")]
    User {
        id: String,
        timestamp: String,
        content: String,
    },
    #[serde(rename = "gemini")]
    Gemini {
        id: String,
        timestamp: String,
        content: String,
        #[serde(default)]
        thoughts: Vec<serde_json::Value>,
        tokens: Option<GeminiTokens>,
        #[serde(rename = "toolCalls", default)]
        tool_calls: Vec<serde_json::Value>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GeminiTokens {
    #[serde(default)]
    input: u64,
    #[serde(default)]
    output: u64,
    #[serde(default)]
    cached: u64,
    #[serde(default)]
    thoughts: u64,
    #[serde(default)]
    tool: u64,
    #[serde(default)]
    total: u64,
}

// Tool extraction and file operation mapping
fn extract_tool_stats(tool_calls: &[serde_json::Value]) -> Stats {
    let mut stats = Stats::default();

    for tool_call in tool_calls {
        if let Some(tool_name) = tool_call.get("name").and_then(|v| v.as_str()) {
            match tool_name {
                "read_many_files" => {
                    if let Some(paths) = tool_call
                        .get("args")
                        .and_then(|v| v.get("paths"))
                        .and_then(|v| v.as_array())
                    {
                        stats.files_read += paths.len() as u64;

                        // Categorize files and estimate composition stats
                        for path in paths {
                            if let Some(path_str) = path.as_str() {
                                let ext = std::path::Path::new(path_str)
                                    .extension()
                                    .and_then(|e| e.to_str())
                                    .unwrap_or("");
                                let category = FileCategory::from_extension(ext);
                                let estimated_lines = 100; // Estimate lines per file
                                match category {
                                    FileCategory::SourceCode => stats.code_lines += estimated_lines,
                                    FileCategory::Documentation => stats.docs_lines += estimated_lines,
                                    FileCategory::Data => stats.data_lines += estimated_lines,
                                    FileCategory::Media => stats.media_lines += estimated_lines,
                                    FileCategory::Config => stats.config_lines += estimated_lines,
                                    FileCategory::Other => stats.other_lines += estimated_lines,
                                }
                            }
                        }

                        // Simple estimation without complex heuristics
                        stats.lines_read += (paths.len() as u64) * 100;
                        stats.bytes_read += (paths.len() as u64) * 8000;
                    }
                }
                "replace" => {
                    stats.files_edited += 1;
                    // Simple counting without complex content analysis
                    stats.lines_edited += 10; // Conservative estimate
                    stats.bytes_edited += 800;
                }
                "run_shell_command" => {
                    stats.terminal_commands += 1;
                }
                "list_directory" => {
                    // Treat as a lightweight read operation
                    stats.files_read += 1;
                }
                _ => {} // Unknown tools - just skip
            }
        }
    }

    // Use existing utility functions for line estimation
    stats.lines_added = (stats.lines_edited / 2).max(1); // Simple estimate
    stats.lines_deleted = (stats.lines_edited / 3).max(1); // Simple estimate

    stats
}

// Helper function to generate hash from conversation file path and timestamp
fn generate_conversation_hash(conversation_file: &str, timestamp: &str) -> String {
    let input = format!("{conversation_file}:{timestamp}");
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..8]) // Use first 8 bytes (16 hex chars) for consistency
}

// Helper function to extract project ID from Gemini file path and hash it
fn extract_and_hash_project_id_gemini(file_path: &Path) -> String {
    // Gemini path format: ~/.gemini/tmp/{PROJECT_ID}/chats/{session}.json
    // Example: "/home/user/.gemini/tmp/project-abc123/chats/session.json"
    
    let path_components: Vec<_> = file_path.components().collect();
    for (i, component) in path_components.iter().enumerate() {
        if let std::path::Component::Normal(name) = component {
            if name.to_str() == Some("tmp") && i + 1 < path_components.len() {
                if let std::path::Component::Normal(project_id) = &path_components[i + 1] {
                    if let Some(project_id_str) = project_id.to_str() {
                        // Hash the project ID using the same algorithm as the rest of the app
                        let mut hasher = Sha256::new();
                        hasher.update(project_id_str.as_bytes());
                        let result = hasher.finalize();
                        return hex::encode(&result[..8]); // Use first 8 bytes (16 hex chars) for consistency
                    }
                }
            }
        }
    }
    
    // Fallback: hash the full file path if we can't extract project ID
    let mut hasher = Sha256::new();
    hasher.update(file_path.to_string_lossy().as_bytes());
    let result = hasher.finalize();
    hex::encode(&result[..8])
}

// Cost calculation with tiered pricing support
fn calculate_gemini_cost(tokens: &GeminiTokens, model_name: &str) -> f64 {
    match MODEL_PRICING.get(model_name) {
        Some(pricing) => {
            let total_input_tokens = tokens.input + tokens.thoughts + tokens.tool;
            let total_output_tokens = tokens.output;

            // Check if this model has Gemini-specific tiered pricing rules
            let input_cost = match &pricing.model_rules {
                crate::models::ModelSpecificRules::Gemini {
                    high_volume_input_cost_per_token,
                    high_volume_threshold,
                    ..
                } => {
                    if total_input_tokens > *high_volume_threshold {
                        if let Some(high_volume_rate) = high_volume_input_cost_per_token {
                            // First threshold amount at normal rate, rest at high volume rate
                            (*high_volume_threshold as f64 * pricing.input_cost_per_token)
                                + ((total_input_tokens - *high_volume_threshold) as f64
                                    * high_volume_rate)
                        } else {
                            // No tiered pricing, use normal rate for all tokens
                            total_input_tokens as f64 * pricing.input_cost_per_token
                        }
                    } else {
                        // Under threshold, use normal rate
                        total_input_tokens as f64 * pricing.input_cost_per_token
                    }
                }
                crate::models::ModelSpecificRules::OpenAI { .. }
                | crate::models::ModelSpecificRules::None => {
                    // No tiered pricing, use normal rate for all tokens
                    total_input_tokens as f64 * pricing.input_cost_per_token
                }
            };

            // Calculate output cost with tiered pricing
            let output_cost = match &pricing.model_rules {
                crate::models::ModelSpecificRules::Gemini {
                    high_volume_output_cost_per_token,
                    high_volume_threshold,
                    ..
                } => {
                    if total_output_tokens > *high_volume_threshold {
                        if let Some(high_volume_rate) = high_volume_output_cost_per_token {
                            // First threshold amount at normal rate, rest at high volume rate
                            (*high_volume_threshold as f64 * pricing.output_cost_per_token)
                                + ((total_output_tokens - *high_volume_threshold) as f64
                                    * high_volume_rate)
                        } else {
                            // No tiered pricing, use normal rate for all tokens
                            total_output_tokens as f64 * pricing.output_cost_per_token
                        }
                    } else {
                        // Under threshold, use normal rate
                        total_output_tokens as f64 * pricing.output_cost_per_token
                    }
                }
                crate::models::ModelSpecificRules::OpenAI { .. }
                | crate::models::ModelSpecificRules::None => {
                    // No tiered pricing, use normal rate for all tokens
                    total_output_tokens as f64 * pricing.output_cost_per_token
                }
            };

            // Cache cost (always at cache read rate)
            let cache_cost = tokens.cached as f64 * pricing.cache_read_input_token_cost;

            input_cost + output_cost + cache_cost
        }
        None => {
            eprintln!("WARNING: Unknown Gemini model: {model_name}. Using default pricing.",);
            // Fallback to default Gemini 2.5 Flash pricing (more conservative)
            calculate_gemini_cost(tokens, "gemini-2.5-flash")
        }
    }
}


// JSON session parsing (not JSONL)
fn parse_json_session_file(file_path: &Path) -> Vec<ConversationMessage> {
    let conversation_file = file_path.to_string_lossy().to_string();
    let project_hash = extract_and_hash_project_id_gemini(file_path);
    let mut entries = Vec::new();

    // Read entire file (not line-by-line like JSONL)
    let file_content = match std::fs::read_to_string(file_path) {
        Ok(content) => content,
        Err(e) => {
            eprintln!(
                "Failed to read Gemini session file {}: {}",
                file_path.display(),
                e
            );
            return entries;
        }
    };

    // Parse the complete session JSON
    let session: GeminiSession = match serde_json::from_str(&file_content) {
        Ok(session) => session,
        Err(e) => {
            eprintln!(
                "Failed to parse Gemini session JSON {}: {}",
                file_path.display(),
                e
            );
            return entries;
        }
    };

    // Process each message in the session
    for message in session.messages {
        match message {
            GeminiMessage::User {
                id: _,
                timestamp,
                content: _,
            } => {
                entries.push(ConversationMessage {
                    timestamp: timestamp.clone(),
                    application: Application::GeminiCLI,
                    hash: generate_conversation_hash(&conversation_file, &timestamp),
                    project_hash: project_hash.clone(),
                    model: None,
                    stats: Stats::default(),
                    role: MessageRole::User,
                });
            }
            GeminiMessage::Gemini {
                id: _,
                timestamp,
                content: _,
                thoughts: _,
                tokens,
                tool_calls,
            } => {
                if let Some(tokens) = tokens {
                    let mut stats = extract_tool_stats(&tool_calls);
                    let hash = generate_conversation_hash(&conversation_file, &timestamp);

                    // Use a reasonable fallback model - Gemini 2.5 Flash is most common and cost-effective
                    let fallback_model = "gemini-2.5-flash";

                    // Update stats with token information
                    stats.input_tokens = tokens.input;
                    stats.output_tokens = tokens.output;
                    stats.cache_creation_tokens = 0;
                    stats.cache_read_tokens = 0;
                    stats.cached_tokens = tokens.cached;
                    stats.cost = calculate_gemini_cost(&tokens, fallback_model);
                    stats.tool_calls = tool_calls.len() as u32;

                    entries.push(ConversationMessage {
                        application: Application::GeminiCLI,
                        model: Some(fallback_model.to_string()), // TODO: Extract actual model from session
                        timestamp,
                        hash,
                        project_hash: project_hash.clone(),
                        stats,
                        role: MessageRole::Assistant,
                    });
                }
            }
        }
    }

    entries
}

#[async_trait]
impl Analyzer for GeminiAnalyzer {
    fn display_name(&self) -> &'static str {
        "Gemini CLI"
    }

    fn get_model_abbreviations(&self) -> ModelAbbreviations {
        let mut abbreviations = ModelAbbreviations::new();
        abbreviations.add(
            "gemini-2.5-pro".to_string(),
            "G2.5P".to_string(),
            "Gemini 2.5 Pro".to_string(),
        );
        abbreviations.add(
            "gemini-2.5-flash".to_string(),
            "G2.5F".to_string(),
            "Gemini 2.5 Flash".to_string(),
        );
        abbreviations.add(
            "gemini-1.5-pro".to_string(),
            "G1.5P".to_string(),
            "Gemini 1.5 Pro".to_string(),
        );
        abbreviations.add(
            "gemini-1.5-flash".to_string(),
            "G1.5F".to_string(),
            "Gemini 1.5 Flash".to_string(),
        );
        abbreviations
    }

    fn get_data_glob_patterns(&self) -> Vec<String> {
        let mut patterns = Vec::new();

        if let Some(home_dir) = std::env::home_dir() {
            let home_str = home_dir.to_string_lossy();
            patterns.push(format!("{home_str}/.gemini/tmp/*/chats/*.json"));
        }

        patterns
    }

    fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let patterns = self.get_data_glob_patterns();
        let mut sources = Vec::new();

        for pattern in patterns {
            for entry in glob(&pattern)? {
                let path = entry?;
                if path.is_file() {
                    sources.push(DataSource { path });
                }
            }
        }

        Ok(sources)
    }

    async fn parse_conversations(
        &self,
        sources: Vec<DataSource>,
    ) -> Result<Vec<ConversationMessage>> {
        // Parse all session files in parallel
        let all_entries: Vec<ConversationMessage> = sources
            .into_par_iter()
            .flat_map(|source| parse_json_session_file(&source.path))
            .collect();

        // Deduplicate based on hash
        let mut seen_hashes = HashSet::new();
        let deduplicated_entries: Vec<ConversationMessage> = all_entries
            .into_iter()
            .filter(|entry| {
                if seen_hashes.contains(&entry.hash) {
                    false
                } else {
                    seen_hashes.insert(entry.hash.clone());
                    true
                }
            })
            .collect();

        Ok(deduplicated_entries)
    }

    async fn get_stats(&self) -> Result<AgenticCodingToolStats> {
        let sources = self.discover_data_sources()?;
        let messages = self.parse_conversations(sources).await?;

        // Group messages by date and calculate daily stats (reusing existing logic)
        let mut daily_stats: BTreeMap<String, DailyStats> = BTreeMap::new();

        for message in &messages {
            let date = if let Ok(parsed_time) = DateTime::parse_from_rfc3339(&message.timestamp) {
                parsed_time.format("%Y-%m-%d").to_string()
            } else {
                // Fallback date parsing
                message.timestamp.split('T').next().unwrap_or("unknown").to_string()
            };


            let daily_stats_entry = daily_stats.entry(date).or_default();

            match message {
                ConversationMessage {
                    model: Some(_),
                    stats,
                    ..
                } => {
                    daily_stats_entry.stats.cost += stats.cost;
                    daily_stats_entry.stats.input_tokens += stats.input_tokens;
                    daily_stats_entry.stats.output_tokens += stats.output_tokens;
                    daily_stats_entry.stats.tool_calls += stats.tool_calls;

                    // Aggregate all stats into daily stats
                    daily_stats_entry.stats.cost += stats.cost;
                    daily_stats_entry.stats.input_tokens += stats.input_tokens;
                    daily_stats_entry.stats.output_tokens += stats.output_tokens;
                    daily_stats_entry.stats.cache_creation_tokens += stats.cache_creation_tokens;
                    daily_stats_entry.stats.cache_read_tokens += stats.cache_read_tokens;
                    daily_stats_entry.stats.cached_tokens += stats.cached_tokens;
                    daily_stats_entry.stats.tool_calls += stats.tool_calls;
                    daily_stats_entry.stats.terminal_commands += stats.terminal_commands;
                    daily_stats_entry.stats.file_searches += stats.file_searches;
                    daily_stats_entry.stats.file_content_searches += stats.file_content_searches;
                    daily_stats_entry.stats.files_read += stats.files_read;
                    daily_stats_entry.stats.files_added += stats.files_added;
                    daily_stats_entry.stats.files_edited += stats.files_edited;
                    daily_stats_entry.stats.files_deleted += stats.files_deleted;
                    daily_stats_entry.stats.lines_read += stats.lines_read;
                    daily_stats_entry.stats.lines_added += stats.lines_added;
                    daily_stats_entry.stats.lines_edited += stats.lines_edited;
                    daily_stats_entry.stats.lines_deleted += stats.lines_deleted;
                    daily_stats_entry.stats.bytes_read += stats.bytes_read;
                    daily_stats_entry.stats.bytes_added += stats.bytes_added;
                    daily_stats_entry.stats.bytes_edited += stats.bytes_edited;
                    daily_stats_entry.stats.bytes_deleted += stats.bytes_deleted;
                    daily_stats_entry.stats.todos_created += stats.todos_created;
                    daily_stats_entry.stats.todos_completed += stats.todos_completed;
                    daily_stats_entry.stats.todos_in_progress += stats.todos_in_progress;
                    daily_stats_entry.stats.todo_writes += stats.todo_writes;
                    daily_stats_entry.stats.todo_reads += stats.todo_reads;
                    daily_stats_entry.stats.code_lines += stats.code_lines;
                    daily_stats_entry.stats.docs_lines += stats.docs_lines;
                    daily_stats_entry.stats.data_lines += stats.data_lines;
                    daily_stats_entry.stats.media_lines += stats.media_lines;
                    daily_stats_entry.stats.config_lines += stats.config_lines;
                    daily_stats_entry.stats.other_lines += stats.other_lines;

                }
                ConversationMessage {
                    model: None, ..
                } => {
                    daily_stats_entry.conversations += 1;
                }
            }
        }

        let num_conversations = messages
            .iter()
            .filter(|m| m.role == MessageRole::User)
            .count() as u64;

        Ok(AgenticCodingToolStats {
            analyzer_name: self.display_name().to_string(),
            daily_stats,
            messages,
            num_conversations,
            model_abbrs: self.get_model_abbreviations(),
        })
    }

    fn is_available(&self) -> bool {
        self.discover_data_sources()
            .is_ok_and(|sources| !sources.is_empty())
    }
}
