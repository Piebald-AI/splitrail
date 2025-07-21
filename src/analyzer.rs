use anyhow::Result;
use std::collections::HashMap;
use std::path::PathBuf;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::types::{AgenticCodingToolStats, ConversationMessage};
use crate::utils::ModelAbbreviations;

/// Represents a data source for an analyzer
#[derive(Debug, Clone)]
pub struct DataSource {
    pub path: PathBuf,
    pub format: DataFormat,
    pub metadata: HashMap<String, String>,
}

/// Supported data formats
#[derive(Debug, Clone)]
pub enum DataFormat {
    JsonL,           // Claude Code uses this
    Json,            // Single JSON files
    Custom(String),  // Tool-specific formats
}

/// Capabilities that an analyzer may or may not support
#[derive(Debug, Clone)]
pub struct AnalyzerCapabilities {
    pub supports_todos: bool,
    pub caching_type: Option<CachingType>,
    pub supports_file_operations: bool,
    pub supports_cost_tracking: bool,
    pub supports_model_selection: bool,
    pub supported_tools: Vec<String>,
}

/// Different types of token caching implementations
#[derive(Debug, Clone)]
pub enum CachingType {
    CreationAndRead,  // Claude Code: separate creation/read tokens
    InputOnly,        // Codex: only cached input tokens
    Generic,          // For tools with simple cached tokens
    ContextCaching,   // Gemini: context caching with volume-based pricing
}

/// Flexible caching information that can represent different tool implementations
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CachingInfo {
    /// Claude Code style: separate creation and read tokens
    #[serde(rename = "creationAndRead")]
    CreationAndRead {
        cache_creation_tokens: u64,
        cache_read_tokens: u64,
    },
    /// Codex style: only cached input tokens
    #[serde(rename = "inputOnly")]
    InputOnly {
        cached_input_tokens: u64,
    },
    /// Generic style: for future tools with simple cached tokens
    #[serde(rename = "generic")]
    Generic {
        cached_tokens: u64,
    },
    /// Gemini style: context caching with volume-based pricing
    #[serde(rename = "contextCaching")]
    ContextCaching {
        cached_tokens: u64,
        /// Whether these tokens are in the high-volume tier (>200k)
        high_volume: bool,
    },
}

impl CachingInfo {
    /// Get the total cached tokens regardless of implementation
    pub fn total_cached_tokens(&self) -> u64 {
        match self {
            CachingInfo::CreationAndRead { cache_creation_tokens, cache_read_tokens } => {
                cache_creation_tokens + cache_read_tokens
            }
            CachingInfo::InputOnly { cached_input_tokens } => *cached_input_tokens,
            CachingInfo::Generic { cached_tokens } => *cached_tokens,
            CachingInfo::ContextCaching { cached_tokens, .. } => *cached_tokens,
        }
    }
}

/// Main trait that all analyzers must implement
#[async_trait]
pub trait Analyzer: Send + Sync {
    /// Get the unique identifier for this analyzer
    fn name(&self) -> &'static str;
    
    /// Get the display name for this analyzer
    fn display_name(&self) -> &'static str;
    
    /// Get the capabilities of this analyzer
    fn get_capabilities(&self) -> AnalyzerCapabilities;
    
    /// Get model abbreviations for this analyzer
    fn get_model_abbreviations(&self) -> ModelAbbreviations;

    /// Get glob patterns for discovering data sources
    fn get_data_glob_patterns(&self) -> Vec<String>;
    
    /// Discover data sources for this analyzer
    fn discover_data_sources(&self) -> Result<Vec<DataSource>>;
    
    /// Parse conversations from data sources into normalized messages
    async fn parse_conversations(&self, sources: Vec<DataSource>) -> Result<Vec<ConversationMessage>>;
    
    /// Get complete statistics for this analyzer
    async fn get_stats(&self) -> Result<AgenticCodingToolStats>;
    
    /// Check if this analyzer is available on the current system
    fn is_available(&self) -> bool;
}

/// Registry for managing multiple analyzers
#[derive(Default)]
pub struct AnalyzerRegistry {
    analyzers: Vec<Box<dyn Analyzer>>,
}

impl AnalyzerRegistry {
    /// Create a new analyzer registry
    pub fn new() -> Self {
        Self {
            analyzers: Vec::new(),
        }
    }
    
    /// Register an analyzer
    pub fn register<A: Analyzer + 'static>(&mut self, analyzer: A) {
        self.analyzers.push(Box::new(analyzer));
    }
    
    /// Get all registered analyzers
    pub fn analyzers(&self) -> &[Box<dyn Analyzer>] {
        &self.analyzers
    }
    
    /// Get available analyzers (those that are present on the system)
    pub fn available_analyzers(&self) -> Vec<&dyn Analyzer> {
        self.analyzers
            .iter()
            .filter(|a| a.is_available())
            .map(|a| a.as_ref())
            .collect()
    }
    
    /// Get analyzer by name
    pub fn get_analyzer(&self, name: &str) -> Option<&dyn Analyzer> {
        self.analyzers
            .iter()
            .find(|a| a.name() == name)
            .map(|a| a.as_ref())
    }
    
    /// Get analyzer by display name  
    pub fn get_analyzer_by_display_name(&self, display_name: &str) -> Option<&dyn Analyzer> {
        self.analyzers
            .iter()
            .find(|a| a.display_name() == display_name)
            .map(|a| a.as_ref())
    }
    
    /// Get the first available analyzer
    pub fn get_primary_analyzer(&self) -> Option<&dyn Analyzer> {
        self.available_analyzers().into_iter().next()
    }
    
    /// Get the analyzer with the most data sources (prioritizes by volume)
    pub fn get_primary_analyzer_by_volume(&self) -> Option<&dyn Analyzer> {
        let mut best_analyzer: Option<&dyn Analyzer> = None;
        let mut best_count: usize = 0;
        
        for analyzer in self.available_analyzers() {
            if let Ok(sources) = analyzer.discover_data_sources() {
                let count = sources.len();
                if count > best_count {
                    best_count = count;
                    best_analyzer = Some(analyzer);
                }
            }
        }
        
        best_analyzer
    }

    /// Load stats from all available analyzers
    pub async fn load_all_stats(&self) -> Result<crate::types::MultiAnalyzerStats> {
        let available_analyzers = self.available_analyzers();
        let mut all_stats = Vec::new();
        
        for analyzer in available_analyzers {
            match analyzer.get_stats().await {
                Ok(stats) => all_stats.push(stats),
                Err(e) => {
                    eprintln!("⚠️  Error analyzing {} data: {}", analyzer.display_name(), e);
                }
            }
        }

        Ok(crate::types::MultiAnalyzerStats {
            analyzer_stats: all_stats,
        })
    }

    /// Get a mapping of data directories to analyzer names for file watching
    pub fn get_directory_to_analyzer_mapping(&self) -> std::collections::HashMap<PathBuf, String> {
        let mut dir_to_analyzer = std::collections::HashMap::new();
        
        for analyzer in self.available_analyzers() {
            if let Ok(sources) = analyzer.discover_data_sources() {
                for source in sources {
                    if let Some(parent) = source.path.parent() {
                        if parent.exists() {
                            dir_to_analyzer.insert(parent.to_path_buf(), analyzer.display_name().to_string());
                        }
                    }
                }
            }
        }
        
        dir_to_analyzer
    }
}