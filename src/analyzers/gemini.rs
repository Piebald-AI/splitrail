use crate::analyzer::{Analyzer, DataSource};
use crate::models::MODEL_PRICING;
use crate::types::{
    AgenticCodingToolStats, Application, CompositionStats, ConversationMessage, DailyStats,
    FileCategory, FileOperationStats, GeneralStats,
};
use crate::utils::ModelAbbreviations;
use anyhow::Result;
use async_trait::async_trait;
use chrono::DateTime;
use glob::glob;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
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
fn extract_tool_stats(tool_calls: &[serde_json::Value]) -> (FileOperationStats, CompositionStats) {
    let mut file_ops = FileOperationStats::default();
    let mut composition_stats = CompositionStats::default();

    for tool_call in tool_calls {
        if let Some(tool_name) = tool_call.get("name").and_then(|v| v.as_str()) {
            match tool_name {
                "read_many_files" => {
                    if let Some(paths) = tool_call
                        .get("args")
                        .and_then(|v| v.get("paths"))
                        .and_then(|v| v.as_array())
                    {
                        file_ops.files_read += paths.len() as u64;

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
                                    FileCategory::SourceCode => composition_stats.code_lines += estimated_lines,
                                    FileCategory::Documentation => composition_stats.docs_lines += estimated_lines,
                                    FileCategory::Data => composition_stats.data_lines += estimated_lines,
                                    FileCategory::Media => composition_stats.media_lines += estimated_lines,
                                    FileCategory::Config => composition_stats.config_lines += estimated_lines,
                                    FileCategory::Other => composition_stats.other_lines += estimated_lines,
                                }
                            }
                        }

                        // Simple estimation without complex heuristics
                        file_ops.lines_read += (paths.len() as u64) * 100;
                        file_ops.bytes_read += (paths.len() as u64) * 8000;
                    }
                }
                "replace" => {
                    file_ops.files_edited += 1;
                    // Simple counting without complex content analysis
                    file_ops.lines_edited += 10; // Conservative estimate
                    file_ops.bytes_edited += 800;
                }
                "run_shell_command" => {
                    file_ops.terminal_commands += 1;
                }
                "list_directory" => {
                    // Treat as a lightweight read operation
                    file_ops.files_read += 1;
                }
                _ => {} // Unknown tools - just skip
            }
        }
    }

    // Use existing utility functions for line estimation
    file_ops.lines_added = (file_ops.lines_edited / 2).max(1); // Simple estimate
    file_ops.lines_deleted = (file_ops.lines_edited / 3).max(1); // Simple estimate

    (file_ops, composition_stats)
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
                id,
                timestamp,
                content: _,
            } => {
                entries.push(ConversationMessage::User {
                    timestamp: timestamp.clone(),
                    application: Application::GeminiCLI,
                    hash: Some(generate_conversation_hash(&conversation_file, &timestamp)),
                    project_hash: project_hash.clone(),
                    todo_stats: None,
                    analyzer_specific: {
                        let mut map = HashMap::new();
                        map.insert("user_id".to_string(), serde_json::Value::String(id));
                        map.insert(
                            "session_id".to_string(),
                            serde_json::Value::String(session.session_id.clone()),
                        );
                        map.insert(
                            "project_hash".to_string(),
                            serde_json::Value::String(session.project_hash.clone()),
                        );
                        map
                    },
                });
            }
            GeminiMessage::Gemini {
                id,
                timestamp,
                content: _,
                thoughts,
                tokens,
                tool_calls,
            } => {
                if let Some(tokens) = tokens {
                    let (file_ops, composition_stats) = extract_tool_stats(&tool_calls);
                    let hash = generate_conversation_hash(&conversation_file, &timestamp);

                    // Use a reasonable fallback model - Gemini 2.5 Flash is most common and cost-effective
                    let fallback_model = "gemini-2.5-flash";

                    entries.push(ConversationMessage::AI {
                        application: Application::GeminiCLI,
                        model: fallback_model.to_string(), // TODO: Extract actual model from session
                        timestamp,
                        hash: Some(hash),
                        project_hash: project_hash.clone(),
                        file_operations: file_ops,
                        todo_stats: None, // Gemini CLI doesn't have todos
                        composition_stats,
                        analyzer_specific: {
                            let mut map = HashMap::new();
                            map.insert("gemini_id".to_string(), serde_json::Value::String(id));
                            map.insert(
                                "session_id".to_string(),
                                serde_json::Value::String(session.session_id.clone()),
                            );
                            map.insert(
                                "project_hash".to_string(),
                                serde_json::Value::String(session.project_hash.clone()),
                            );
                            map.insert(
                                "thoughts_tokens".to_string(),
                                serde_json::Value::Number(tokens.thoughts.into()),
                            );
                            map.insert(
                                "tool_tokens".to_string(),
                                serde_json::Value::Number(tokens.tool.into()),
                            );
                            map.insert(
                                "thoughts".to_string(),
                                serde_json::to_value(thoughts).unwrap_or_default(),
                            );
                            map.insert(
                                "tool_calls".to_string(),
                                serde_json::to_value(&tool_calls).unwrap_or_default(),
                            );
                            map
                        },
                        general_stats: GeneralStats {
                            input_tokens: tokens.input,
                            output_tokens: tokens.output,
                            cache_creation_tokens: 0,
                            cache_read_tokens: 0,
                            cached_tokens: tokens.cached,
                            cost: calculate_gemini_cost(&tokens, fallback_model),
                            tool_calls: tool_calls.len() as u32,
                        },
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
                if let ConversationMessage::AI {
                    hash: Some(hash), ..
                } = entry
                {
                    if seen_hashes.contains(hash) {
                        false
                    } else {
                        seen_hashes.insert(hash.clone());
                        true
                    }
                } else {
                    true // Keep all user messages
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
            let date = match message {
                ConversationMessage::AI { timestamp, .. }
                | ConversationMessage::User { timestamp, .. } => {
                    // Extract date from timestamp
                    if let Ok(parsed_time) = DateTime::parse_from_rfc3339(timestamp) {
                        parsed_time.format("%Y-%m-%d").to_string()
                    } else {
                        // Fallback date parsing
                        timestamp.split('T').next().unwrap_or("unknown").to_string()
                    }
                }
            };

            let stats = daily_stats.entry(date).or_default();

            match message {
                ConversationMessage::AI {
                    general_stats,
                    file_operations,
                    composition_stats,
                    ..
                } => {
                    stats.cost += general_stats.cost;
                    stats.input_tokens += general_stats.input_tokens;
                    stats.output_tokens += general_stats.output_tokens;
                    stats.tool_calls += general_stats.tool_calls;

                    // Aggregate file operations
                    stats.file_operations.files_read += file_operations.files_read;
                    stats.file_operations.files_edited += file_operations.files_edited;
                    stats.file_operations.files_added += file_operations.files_added;
                    stats.file_operations.files_deleted += file_operations.files_deleted;
                    stats.file_operations.terminal_commands += file_operations.terminal_commands;
                    stats.file_operations.file_searches += file_operations.file_searches;
                    stats.file_operations.file_content_searches +=
                        file_operations.file_content_searches;
                    stats.file_operations.lines_read += file_operations.lines_read;
                    stats.file_operations.lines_added += file_operations.lines_added;
                    stats.file_operations.lines_edited += file_operations.lines_edited;
                    stats.file_operations.lines_deleted += file_operations.lines_deleted;
                    stats.file_operations.bytes_read += file_operations.bytes_read;
                    stats.file_operations.bytes_added += file_operations.bytes_added;
                    stats.file_operations.bytes_edited += file_operations.bytes_edited;
                    stats.file_operations.bytes_deleted += file_operations.bytes_deleted;

                    // Aggregate composition stats
                    stats.composition_stats.code_lines += composition_stats.code_lines;
                    stats.composition_stats.docs_lines += composition_stats.docs_lines;
                    stats.composition_stats.data_lines += composition_stats.data_lines;
                    stats.composition_stats.media_lines += composition_stats.media_lines;
                    stats.composition_stats.config_lines += composition_stats.config_lines;
                    stats.composition_stats.other_lines += composition_stats.other_lines;

                }
                ConversationMessage::User { .. } => {
                    stats.conversations += 1;
                }
            }
        }

        let num_conversations = messages
            .iter()
            .filter(|m| matches!(m, ConversationMessage::User { .. }))
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
