use phf::phf_map;
use serde::{Deserialize, Serialize};

use crate::utils::warn_once;

/// Represents different pricing tier structures for various models
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PricingTier {
    /// Maximum tokens for this tier (None means unlimited - highest tier)
    pub max_tokens: Option<u64>,
    /// Input cost per 1M tokens
    pub input_per_1m: f64,
    /// Output cost per 1M tokens
    pub output_per_1m: f64,
}

/// Different pricing structures supported by various model providers
#[derive(Debug, Clone)]
pub enum PricingStructure {
    /// Flat rate pricing (same cost regardless of token count)
    Flat {
        input_per_1m: f64,
        output_per_1m: f64,
    },
    /// Tiered pricing (different costs based on token thresholds)
    Tiered { tiers: &'static [PricingTier] },
}

/// Caching tier for models with tiered cache pricing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachingTier {
    /// Maximum tokens for this caching tier (None means unlimited)
    pub max_tokens: Option<u64>,
    /// Cached input cost per 1M tokens
    pub cached_input_per_1m: f64,
}

/// Different caching support models
#[derive(Debug, Clone)]
pub enum CachingSupport {
    /// Model does not support caching
    None,
    /// OpenAI-style caching (simple cached input pricing)
    OpenAI { cached_input_per_1m: f64 },
    /// Anthropic-style caching (separate write and read costs)
    Anthropic {
        cache_write_per_1m: f64,
        cache_read_per_1m: f64,
    },
    /// Google-style caching (may have tiers like input/output)
    Google { tiers: &'static [CachingTier] },
}

/// Complete model information with all pricing details
#[derive(Debug, Clone)]
pub struct ModelInfo {
    /// Pricing structure (flat or tiered)
    pub pricing: PricingStructure,
    /// Caching support and pricing
    pub caching: CachingSupport,
}

static MODEL_INDEX: phf::Map<&'static str, ModelInfo> = phf_map! {
    // OpenAI Models
    "o4-mini" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 1.1,
            output_per_1m: 4.4,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.275,
        },
    },
    "o3" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 2.0,
            output_per_1m: 8.0,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.5,
        },
    },
    "o3-pro" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 20.0,
            output_per_1m: 80.0,
        },
        caching: CachingSupport::None,
    },
    "o3-mini" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 1.1,
            output_per_1m: 4.4,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.55,
        },
    },
    "o1" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 60.0,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 7.5,
        },
    },
    "o1-preview" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 60.0,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 7.5,
        },
    },
    "o1-mini" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 1.1,
            output_per_1m: 4.4,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.55,
        },
    },
    "o1-pro" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 150.0,
            output_per_1m: 600.0,
        },
        caching: CachingSupport::None,
    },
    "gpt-4.1" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 2.0,
            output_per_1m: 8.0,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.5,
        },
    },
    "gpt-4o" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 2.5,
            output_per_1m: 10.0,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 1.25,
        },
    },
    "gpt-4o-2024-05-13" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 5.0,
            output_per_1m: 10.0,
        },
        caching: CachingSupport::None,
    },
    "gpt-4.1-mini" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.4,
            output_per_1m: 1.6,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.1,
        },
    },
    "gpt-4.1-nano" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.1,
            output_per_1m: 0.4,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.025,
        },
    },
    "gpt-4o-mini" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.15,
            output_per_1m: 0.6,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.075,
        },
    },
    "codex-mini-latest" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 1.5,
            output_per_1m: 6.0,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.375,
        },
    },
    "gpt-4-turbo" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 10.0,
            output_per_1m: 30.0,
        },
        caching: CachingSupport::None,
    },
    "gpt-5" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 1.25,
            output_per_1m: 10.0,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.125,
        },
    },
    "gpt-5-mini" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.25,
            output_per_1m: 2.0,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.025,
        },
    },
    "gpt-5-nano" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.05,
            output_per_1m: 0.4,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.005,
        },
    },
    "gpt-5-codex-mini" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.25,
            output_per_1m: 2.0,
        },
        caching: CachingSupport::OpenAI {
            cached_input_per_1m: 0.025,
        },
    },

    // Anthropic Models
    "claude-opus-4-1" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 75.0,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 18.75,
            cache_read_per_1m: 1.5,
        },
    },
    "claude-opus-4" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 75.0,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 18.75,
            cache_read_per_1m: 1.5,
        },
    },
    "claude-sonnet-4" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 3.75,
            cache_read_per_1m: 0.3,
        },
    },
    "claude-sonnet-4-5" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 3.75,
            cache_read_per_1m: 0.3,
        },
    },
    "claude-3-7-sonnet" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 3.75,
            cache_read_per_1m: 0.3,
        },
    },
    "claude-3-5-sonnet" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 3.75,
            cache_read_per_1m: 0.3,
        },
    },
    "claude-3-5-haiku" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.8,
            output_per_1m: 4.0,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 1.0,
            cache_read_per_1m: 0.08,
        },
    },
    "claude-haiku-4-5" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 1.0,
            output_per_1m: 5.0,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 1.25,
            cache_read_per_1m: 0.10,
        },
    },
    "claude-3-opus" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 75.0,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 18.75,
            cache_read_per_1m: 1.5,
        },
    },
    "claude-3-haiku" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.25,
            output_per_1m: 1.25,
        },
        caching: CachingSupport::Anthropic {
            cache_write_per_1m: 0.3,
            cache_read_per_1m: 0.03,
        },
    },

    // Google Models
    "gemini-3-pro-preview-11-2025" => ModelInfo {
        pricing: PricingStructure::Tiered {
            tiers: &[
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 1.25,
                    output_per_1m: 10.0,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.5,
                    output_per_1m: 15.0,
                },
            ],
        },
        caching: CachingSupport::Google {
            tiers: &[
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.31,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.625,
                },
            ],
        },
    },
    "gemini-2.5-pro" => ModelInfo {
        pricing: PricingStructure::Tiered {
            tiers: &[
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 1.25,
                    output_per_1m: 10.0,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.5,
                    output_per_1m: 15.0,
                },
            ],
        },
        caching: CachingSupport::Google {
            tiers: &[
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.31,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.625,
                },
            ],
        },
    },
    "gemini-2.5-flash" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.3,
            output_per_1m: 2.5,
        },
        caching: CachingSupport::Google {
            tiers: &[
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.075,
                },
            ],
        },
    },
    "gemini-2.5-flash-lite" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.1,
            output_per_1m: 0.4,
        },
        caching: CachingSupport::Google {
            tiers: &[
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.025,
                },
            ],
        },
    },
    "gemini-2.0-pro-exp-02-05" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.0,
            output_per_1m: 0.0,
        },
        caching: CachingSupport::Google {
            tiers: &[
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.0,
                },
            ],
        },
    },
    "gemini-2.0-flash" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.1,
            output_per_1m: 0.4,
        },
        caching: CachingSupport::Google {
            tiers: &[
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.025,
                },
            ],
        },
    },
    "gemini-2.0-flash-lite" => ModelInfo {
        pricing: PricingStructure::Flat {
            input_per_1m: 0.075,
            output_per_1m: 0.3,
        },
        caching: CachingSupport::None,
    },
    "gemini-1.5-flash" => ModelInfo {
        pricing: PricingStructure::Tiered {
            tiers: &[
                PricingTier {
                    max_tokens: Some(128_000),
                    input_per_1m: 0.075,
                    output_per_1m: 0.3,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 0.15,
                    output_per_1m: 0.6,
                },
            ],
        },
        caching: CachingSupport::Google {
            tiers: &[
                CachingTier {
                    max_tokens: Some(128_000),
                    cached_input_per_1m: 0.01875,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.0375,
                },
            ],
        },
    },
    "gemini-1.5-flash-8b" => ModelInfo {
        pricing: PricingStructure::Tiered {
            tiers: &[
                PricingTier {
                    max_tokens: Some(128_000),
                    input_per_1m: 0.0375,
                    output_per_1m: 0.15,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 0.075,
                    output_per_1m: 0.3,
                },
            ],
        },
        caching: CachingSupport::Google {
            tiers: &[
                CachingTier {
                    max_tokens: Some(128_000),
                    cached_input_per_1m: 0.01,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.02,
                },
            ],
        },
    },
    "gemini-1.5-pro" => ModelInfo {
        pricing: PricingStructure::Tiered {
            tiers: &[
                PricingTier {
                    max_tokens: Some(128_000),
                    input_per_1m: 1.25,
                    output_per_1m: 5.0,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.5,
                    output_per_1m: 10.0,
                },
            ],
        },
        caching: CachingSupport::Google {
            tiers: &[
                CachingTier {
                    max_tokens: Some(128_000),
                    cached_input_per_1m: 0.3125,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.625,
                },
            ],
        },
    },
};

static MODEL_ALIASES: phf::Map<&'static str, &'static str> = phf_map! {
    // OpenAI aliases
    "o4-mini" => "o4-mini",
    "o4-mini-2025-04-16" => "o4-mini",
    "o3" => "o3",
    "o3-2025-04-16" => "o3",
    "o3-pro" => "o3-pro",
    "o3-pro-2025-06-10" => "o3-pro",
    "o3-mini" => "o3-mini",
    "o3-mini-2025-01-31" => "o3-mini",
    "o1" => "o1",
    "o1-2024-12-17" => "o1",
    "o1-preview" => "o1-preview",
    "o1-preview-2024-09-12" => "o1-preview",
    "o1-mini" => "o1-mini",
    "o1-mini-2024-09-12" => "o1-mini",
    "o1-pro" => "o1-pro",
    "o1-pro-2025-03-19" => "o1-pro",
    "gpt-4.1" => "gpt-4.1",
    "gpt-4.1-2025-04-14" => "gpt-4.1",
    "gpt-4o" => "gpt-4o",
    "gpt-4o-2024-11-20" => "gpt-4o",
    "gpt-4o-2024-08-06" => "gpt-4o",
    "gpt-4o-2024-05-13" => "gpt-4o-2024-05-13",
    "gpt-4.1-mini" => "gpt-4.1-mini",
    "gpt-4.1-mini-2025-04-14" => "gpt-4.1-mini",
    "gpt-4.1-nano" => "gpt-4.1-nano",
    "gpt-4.1-nano-2025-04-14" => "gpt-4.1-nano",
    "gpt-4o-mini" => "gpt-4o-mini",
    "gpt-4o-mini-2024-07-18" => "gpt-4o-mini",
    "codex-mini-latest" => "codex-mini-latest",
    "gpt-4-turbo" => "gpt-4-turbo",
    "gpt-4-turbo-2024-04-09" => "gpt-4-turbo",
    "gpt-5" => "gpt-5",
    "gpt-5-codex" => "gpt-5",
    "gpt-5-2025-08-07" => "gpt-5",
    "gpt-5-mini" => "gpt-5-mini",
    "gpt-5-mini-2025-08-07" => "gpt-5-mini",
    "gpt-5-nano" => "gpt-5-nano",
    "gpt-5-nano-2025-08-07" => "gpt-5-nano",
    "gpt-5-codex-mini" => "gpt-5-codex-mini",

    // Anthropic aliases
    "claude-opus-4" => "claude-opus-4",
    "claude-opus-4-20250514" => "claude-opus-4",
    "claude-opus-4-0" => "claude-opus-4",
    "claude-opus-4.1" => "claude-opus-4-1",
    "claude-opus-4-1-20250805" => "claude-opus-4-1",
    "claude-sonnet-4" => "claude-sonnet-4",
    "claude-sonnet-4-20250514" => "claude-sonnet-4",
    "claude-sonnet-4-0" => "claude-sonnet-4",
    "claude-sonnet-4.5" => "claude-sonnet-4-5",
    "claude-sonnet-4-5" => "claude-sonnet-4-5",
    "claude-sonnet-4-5-20250929" => "claude-sonnet-4-5",
    "claude-3-7-sonnet" => "claude-3-7-sonnet",
    "claude-3-7-sonnet-20250219" => "claude-3-7-sonnet",
    "claude-3-7-sonnet-latest" => "claude-3-7-sonnet",
    "claude-3-5-sonnet" => "claude-3-5-sonnet",
    "claude-3-5-sonnet-20241022" => "claude-3-5-sonnet",
    "claude-3-5-sonnet-latest" => "claude-3-5-sonnet",
    "claude-3-5-sonnet-20240620" => "claude-3-5-sonnet",
    "claude-3-5-haiku" => "claude-3-5-haiku",
    "claude-3-5-haiku-20241022" => "claude-3-5-haiku",
    "claude-3-5-haiku-latest" => "claude-3-5-haiku",
    "claude-haiku-4-5" => "claude-haiku-4-5",
    "claude-haiku-4.5" => "claude-haiku-4-5",
    "claude-haiku-4-5-20251001" => "claude-haiku-4-5",
    "claude-3-opus" => "claude-3-opus",
    "claude-3-opus-20240229" => "claude-3-opus",
    "claude-3-haiku" => "claude-3-haiku",
    "claude-3-haiku-20240307" => "claude-3-haiku",

    // Google aliases
    "gemini-3-pro-preview-11-2025" => "gemini-3-pro-preview-11-2025",
    "gemini-2.5-pro" => "gemini-2.5-pro",
    "gemini-2.5-pro-preview-06-05" => "gemini-2.5-pro",
    "gemini-2.5-pro-preview-05-06" => "gemini-2.5-pro",
    "gemini-2.5-pro-preview-03-25" => "gemini-2.5-pro",
    "gemini-2.5-flash" => "gemini-2.5-flash",
    "gemini-2.5-flash-preview-05-20" => "gemini-2.5-flash",
    "gemini-2.5-flash-preview-04-17" => "gemini-2.5-flash",
    "gemini-2.5-flash-lite" => "gemini-2.5-flash-lite",
    "gemini-2.5-flash-lite-06-17" => "gemini-2.5-flash-lite",
    "gemini-2.0-pro-exp-02-05" => "gemini-2.0-pro-exp-02-05",
    "gemini-exp-1206" => "gemini-2.0-pro-exp-02-05",
    "gemini-2.0-flash" => "gemini-2.0-flash",
    "gemini-2.0-flash-001" => "gemini-2.0-flash",
    "gemini-2.0-flash-exp" => "gemini-2.0-flash",
    "gemini-2.0-flash-lite" => "gemini-2.0-flash-lite",
    "gemini-2.0-flash-lite-001" => "gemini-2.0-flash-lite",
    "gemini-1.5-flash" => "gemini-1.5-flash",
    "gemini-1.5-flash-latest" => "gemini-1.5-flash",
    "gemini-1.5-flash-001" => "gemini-1.5-flash",
    "gemini-1.5-flash-002" => "gemini-1.5-flash",
    "gemini-1.5-flash-8b" => "gemini-1.5-flash-8b",
    "gemini-1.5-flash-8b-latest" => "gemini-1.5-flash-8b",
    "gemini-1.5-flash-8b-001" => "gemini-1.5-flash-8b",
    "gemini-1.5-flash-8b-exp-0924" => "gemini-1.5-flash-8b",
    "gemini-1.5-flash-8b-exp-0827" => "gemini-1.5-flash-8b",
    "gemini-1.5-pro" => "gemini-1.5-pro",
    "gemini-1.5-pro-latest" => "gemini-1.5-pro",
    "gemini-1.5-pro-001" => "gemini-1.5-pro",
    "gemini-1.5-pro-002" => "gemini-1.5-pro",
    "gemini-1.5-pro-exp-0827" => "gemini-1.5-pro",
    "gemini-1.5-pro-exp-0801" => "gemini-1.5-pro",
};

/// Get model info by any valid name (canonical or alias)
pub fn get_model_info(model_name: &str) -> Option<&ModelInfo> {
    // First try direct lookup in model index
    if let Some(model_info) = MODEL_INDEX.get(model_name) {
        return Some(model_info);
    }

    // Then try alias lookup
    if let Some(&canonical_name) = MODEL_ALIASES.get(model_name) {
        return MODEL_INDEX.get(canonical_name);
    }

    None
}

/// Calculate cost for input tokens using the model's pricing structure
pub fn calculate_input_cost(model_name: &str, input_tokens: u64) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => match &model_info.pricing {
            PricingStructure::Flat { input_per_1m, .. } => {
                (input_tokens as f64 / 1_000_000.0) * input_per_1m
            }
            PricingStructure::Tiered { tiers } => calculate_tiered_cost(input_tokens, tiers, true),
        },
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            (input_tokens as f64 / 1_000_000.0) * 0.0 // $0 per 1M tokens fallback
        }
    }
}

/// Calculate cost for output tokens using the model's pricing structure
pub fn calculate_output_cost(model_name: &str, output_tokens: u64) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => match &model_info.pricing {
            PricingStructure::Flat { output_per_1m, .. } => {
                (output_tokens as f64 / 1_000_000.0) * output_per_1m
            }
            PricingStructure::Tiered { tiers } => {
                calculate_tiered_cost(output_tokens, tiers, false)
            }
        },
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            (output_tokens as f64 / 1_000_000.0) * 0.0 // $0 per 1M tokens fallback
        }
    }
}

/// Calculate cost for cached tokens
pub fn calculate_cache_cost(
    model_name: &str,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => {
            match &model_info.caching {
                CachingSupport::None => 0.0,
                CachingSupport::OpenAI {
                    cached_input_per_1m,
                } => {
                    // OpenAI only has cached input cost, no creation cost
                    (cache_read_tokens as f64 / 1_000_000.0) * cached_input_per_1m
                }
                CachingSupport::Anthropic {
                    cache_write_per_1m,
                    cache_read_per_1m,
                } => {
                    let creation_cost =
                        (cache_creation_tokens as f64 / 1_000_000.0) * cache_write_per_1m;
                    let read_cost = (cache_read_tokens as f64 / 1_000_000.0) * cache_read_per_1m;
                    creation_cost + read_cost
                }
                CachingSupport::Google { tiers } => {
                    // Google only has read cost, calculate based on tiers
                    calculate_tiered_cache_cost(cache_read_tokens, tiers)
                }
            }
        }
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            (cache_read_tokens as f64 / 1_000_000.0) * 0.0 // $0 per 1M tokens fallback
        }
    }
}

/// Calculate total cost for a model usage
pub fn calculate_total_cost(
    model_name: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    let input_cost = calculate_input_cost(model_name, input_tokens);
    let output_cost = calculate_output_cost(model_name, output_tokens);
    let cache_cost = calculate_cache_cost(model_name, cache_creation_tokens, cache_read_tokens);

    input_cost + output_cost + cache_cost
}

fn calculate_tiered_cost(tokens: u64, tiers: &[PricingTier], is_input: bool) -> f64 {
    let mut total_cost = 0.0;
    let mut remaining_tokens = tokens;

    for tier in tiers {
        if remaining_tokens == 0 {
            break;
        }

        let tier_limit = tier.max_tokens.unwrap_or(u64::MAX);
        let tokens_in_tier = remaining_tokens.min(tier_limit);

        let rate = if is_input {
            tier.input_per_1m
        } else {
            tier.output_per_1m
        };
        total_cost += (tokens_in_tier as f64 / 1_000_000.0) * rate;

        remaining_tokens = remaining_tokens.saturating_sub(tokens_in_tier);
    }

    total_cost
}

fn calculate_tiered_cache_cost(tokens: u64, tiers: &[CachingTier]) -> f64 {
    let mut total_cost = 0.0;
    let mut remaining_tokens = tokens;

    for tier in tiers {
        if remaining_tokens == 0 {
            break;
        }

        let tier_limit = tier.max_tokens.unwrap_or(u64::MAX);
        let tokens_in_tier = remaining_tokens.min(tier_limit);

        total_cost += (tokens_in_tier as f64 / 1_000_000.0) * tier.cached_input_per_1m;

        remaining_tokens = remaining_tokens.saturating_sub(tokens_in_tier);
    }

    total_cost
}
