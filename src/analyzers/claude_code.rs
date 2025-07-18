use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::analyzer::{Analyzer, AnalyzerCapabilities, CachingInfo, CachingType, DataFormat, DataSource};
use crate::types::{AgenticCodingToolStats, ConversationMessage};
use crate::utils::ModelAbbreviations;

pub struct ClaudeCodeAnalyzer;

impl ClaudeCodeAnalyzer {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Analyzer for ClaudeCodeAnalyzer {
    fn name(&self) -> &'static str {
        "claude_code"
    }
    
    fn display_name(&self) -> &'static str {
        "Claude Code"
    }
    
    fn get_capabilities(&self) -> AnalyzerCapabilities {
        AnalyzerCapabilities {
            supports_todos: true,
            caching_type: Some(CachingType::CreationAndRead),
            supports_file_operations: true,
            supports_cost_tracking: true,
            supports_model_selection: true,
            supported_tools: vec![
                "Read".to_string(),
                "Edit".to_string(),
                "MultiEdit".to_string(),
                "Write".to_string(),
                "Bash".to_string(),
                "Glob".to_string(),
                "Grep".to_string(),
                "TodoWrite".to_string(),
                "TodoRead".to_string(),
            ],
        }
    }
    
    fn get_model_abbreviations(&self) -> ModelAbbreviations {
        crate::claude_code::model_abbrs()
    }
    
    fn get_data_directory_pattern(&self) -> &str {
        "~/.claude/projects/**/*.jsonl"
    }
    
    async fn discover_data_sources(&self) -> Result<Vec<DataSource>> {
        let claude_dirs = crate::claude_code::find_claude_dirs();
        let mut sources = Vec::new();
        
        for claude_dir in claude_dirs {
            for entry in glob::glob(&format!("{}/**/*.jsonl", claude_dir.display()))? {
                let path = entry?;
                sources.push(DataSource {
                    path,
                    format: DataFormat::JsonL,
                    metadata: std::collections::HashMap::new(),
                });
            }
        }
        
        Ok(sources)
    }
    
    async fn parse_conversations(&self, sources: Vec<DataSource>) -> Result<Vec<ConversationMessage>> {
        use rayon::iter::{IntoParallelIterator, ParallelIterator};
        
        // Parse all the files in parallel
        let all_entries: Vec<ConversationMessage> = sources
            .into_par_iter()
            .flat_map(|source| crate::claude_code::parse_jsonl_file(&source.path))
            .collect();

        // Deduplicate messages
        let mut seen_hashes = HashSet::new();
        let deduplicated_entries: Vec<ConversationMessage> = all_entries
            .into_iter()
            .filter(|entry| {
                if let ConversationMessage::AI { hash, .. } = &entry {
                    if let Some(hash) = hash {
                        if seen_hashes.contains(hash) {
                            false
                        } else {
                            seen_hashes.insert(hash.clone());
                            true
                        }
                    } else {
                        true
                    }
                } else {
                    true // Keep user messages and entries without hashes
                }
            })
            .collect();

        Ok(deduplicated_entries)
    }
    
    async fn get_stats(&self) -> Result<AgenticCodingToolStats> {
        let sources = self.discover_data_sources().await?;
        let messages = self.parse_conversations(sources).await?;
        let daily_stats = crate::utils::aggregate_by_date(&messages);

        let num_conversations = daily_stats
            .values()
            .map(|stats| stats.conversations as u64)
            .sum();

        Ok(AgenticCodingToolStats {
            daily_stats,
            num_conversations,
            model_abbrs: self.get_model_abbreviations(),
            messages,
            analyzer_name: self.display_name().to_string(),
        })
    }
    
    fn is_available(&self) -> bool {
        !crate::claude_code::find_claude_dirs().is_empty()
    }
}