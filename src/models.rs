use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TieredPricing {
    /// Pricing tiers ordered from lowest threshold to highest.
    pub tiers: Vec<PricingTier>,
    /// If true, bill the entire token count at the single matching tier's rate.
    pub bracket_pricing: bool,
}

/// Different pricing structures supported by various model providers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PricingStructure {
    /// Flat rate pricing (same cost regardless of token count)
    Flat {
        input_per_1m: f64,
        output_per_1m: f64,
    },
    /// Tiered pricing (different costs based on token thresholds)
    Tiered(TieredPricing),
}

/// Caching tier for models with tiered cache pricing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachingTier {
    /// Maximum tokens for this caching tier (None means unlimited)
    pub max_tokens: Option<u64>,
    /// Cached input cost per 1M tokens
    pub cached_input_per_1m: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TieredCaching {
    /// Cache tiers ordered from lowest threshold to highest.
    pub tiers: Vec<CachingTier>,
    /// If true, bill the entire token count at the single matching tier's rate.
    pub bracket_pricing: bool,
}

/// Different caching support models
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    Google(TieredCaching),
}

/// Complete model information with all pricing details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Pricing structure (flat or tiered)
    pub pricing: PricingStructure,
    /// Caching support and pricing
    pub caching: CachingSupport,
    /// Whether pricing is estimated (not officially published by provider)
    pub is_estimated: bool,
}

/// Global registry for models and aliases
struct Registry {
    index: HashMap<String, Arc<ModelInfo>>,
    aliases: HashMap<String, String>,
}

impl Registry {
    fn new_with_defaults() -> Self {
        let mut index = HashMap::new();
        let mut aliases = HashMap::new();
        populate_defaults(&mut index, &mut aliases);
        Self { index, aliases }
    }

    fn merge(
        &mut self,
        external_models: HashMap<String, ModelInfo>,
        external_aliases: HashMap<String, String>,
    ) {
        for (name, info) in external_models {
            if !Self::validate_model_info(&info) {
                warn_once(format!(
                    "WARNING: init_external_models ignoring invalid tier config for model `{name}`."
                ));
                continue;
            }
            self.index.insert(name, Arc::new(info));
        }
        for (alias, canonical) in external_aliases {
            self.aliases.insert(alias, canonical);
        }
    }

    fn validate_model_info(info: &ModelInfo) -> bool {
        let pricing_ok = match &info.pricing {
            PricingStructure::Flat { .. } => true,
            PricingStructure::Tiered(tiered) => {
                Self::validate_tier_bounds(&tiered.tiers, |tier| tier.max_tokens)
            }
        };

        let caching_ok = match &info.caching {
            CachingSupport::Google(tiered) => {
                Self::validate_tier_bounds(&tiered.tiers, |tier| tier.max_tokens)
            }
            _ => true,
        };

        pricing_ok && caching_ok
    }

    fn validate_tier_bounds<T, F>(tiers: &[T], max_tokens: F) -> bool
    where
        F: Fn(&T) -> Option<u64>,
    {
        if tiers.is_empty() {
            return false;
        }

        let mut previous_limit = 0_u64;

        for (index, tier) in tiers.iter().enumerate() {
            match max_tokens(tier) {
                Some(limit) if limit > previous_limit && index + 1 < tiers.len() => {
                    previous_limit = limit;
                }
                None if index + 1 == tiers.len() => return true,
                _ => return false,
            }
        }

        false
    }
}

static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();
static FREE_MODEL_INFO: OnceLock<Arc<ModelInfo>> = OnceLock::new();

/// Merge external model configuration into the global registry.
pub fn init_external_models(
    external_models: HashMap<String, ModelInfo>,
    external_aliases: HashMap<String, String>,
) {
    let rwlock = REGISTRY.get_or_init(|| RwLock::new(Registry::new_with_defaults()));
    let mut registry = rwlock.write();
    registry.merge(external_models, external_aliases);
}

fn get_registry_lock() -> &'static RwLock<Registry> {
    REGISTRY.get_or_init(|| RwLock::new(Registry::new_with_defaults()))
}

fn populate_defaults(
    index: &mut HashMap<String, Arc<ModelInfo>>,
    aliases: &mut HashMap<String, String>,
) {
    macro_rules! add_model {
        ($name:expr, $pricing:expr, $caching:expr, $est:expr) => {
            index.insert(
                $name.to_string(),
                Arc::new(ModelInfo {
                    pricing: $pricing,
                    caching: $caching,
                    is_estimated: $est,
                }),
            );
        };
    }

    // OpenAI Models
    add_model!(
        "o4-mini",
        PricingStructure::Flat {
            input_per_1m: 1.1,
            output_per_1m: 4.4
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.275
        },
        false
    );
    add_model!(
        "o3",
        PricingStructure::Flat {
            input_per_1m: 2.0,
            output_per_1m: 8.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.5
        },
        false
    );
    add_model!(
        "o3-pro",
        PricingStructure::Flat {
            input_per_1m: 20.0,
            output_per_1m: 80.0
        },
        CachingSupport::None,
        false
    );
    add_model!(
        "o3-mini",
        PricingStructure::Flat {
            input_per_1m: 1.1,
            output_per_1m: 4.4
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.55
        },
        false
    );
    add_model!(
        "o1",
        PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 60.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 7.5
        },
        false
    );
    add_model!(
        "o1-preview",
        PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 60.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 7.5
        },
        false
    );
    add_model!(
        "o1-mini",
        PricingStructure::Flat {
            input_per_1m: 1.1,
            output_per_1m: 4.4
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.55
        },
        false
    );
    add_model!(
        "o1-pro",
        PricingStructure::Flat {
            input_per_1m: 150.0,
            output_per_1m: 600.0
        },
        CachingSupport::None,
        false
    );
    add_model!(
        "gpt-4.1",
        PricingStructure::Flat {
            input_per_1m: 2.0,
            output_per_1m: 8.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.5
        },
        false
    );
    add_model!(
        "gpt-4o",
        PricingStructure::Flat {
            input_per_1m: 2.5,
            output_per_1m: 10.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 1.25
        },
        false
    );
    add_model!(
        "gpt-4o-2024-05-13",
        PricingStructure::Flat {
            input_per_1m: 5.0,
            output_per_1m: 10.0
        },
        CachingSupport::None,
        false
    );
    add_model!(
        "gpt-4.1-mini",
        PricingStructure::Flat {
            input_per_1m: 0.4,
            output_per_1m: 1.6
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.1
        },
        false
    );
    add_model!(
        "gpt-4.1-nano",
        PricingStructure::Flat {
            input_per_1m: 0.1,
            output_per_1m: 0.4
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.025
        },
        false
    );
    add_model!(
        "gpt-4o-mini",
        PricingStructure::Flat {
            input_per_1m: 0.15,
            output_per_1m: 0.6
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.075
        },
        false
    );
    add_model!(
        "codex-mini-latest",
        PricingStructure::Flat {
            input_per_1m: 1.5,
            output_per_1m: 6.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.375
        },
        false
    );
    add_model!(
        "gpt-4-turbo",
        PricingStructure::Flat {
            input_per_1m: 10.0,
            output_per_1m: 30.0
        },
        CachingSupport::None,
        false
    );
    add_model!(
        "gpt-4.5",
        PricingStructure::Flat {
            input_per_1m: 75.0,
            output_per_1m: 150.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 37.5
        },
        false
    );
    add_model!(
        "gpt-5",
        PricingStructure::Flat {
            input_per_1m: 1.25,
            output_per_1m: 10.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.125
        },
        false
    );
    add_model!(
        "gpt-5.1",
        PricingStructure::Flat {
            input_per_1m: 1.25,
            output_per_1m: 10.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.125
        },
        false
    );
    add_model!(
        "gpt-5-mini",
        PricingStructure::Flat {
            input_per_1m: 0.25,
            output_per_1m: 2.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.025
        },
        false
    );
    add_model!(
        "gpt-5-nano",
        PricingStructure::Flat {
            input_per_1m: 0.05,
            output_per_1m: 0.4
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.005
        },
        false
    );
    add_model!(
        "gpt-5-codex-mini",
        PricingStructure::Flat {
            input_per_1m: 0.25,
            output_per_1m: 2.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.025
        },
        false
    );
    add_model!(
        "gpt-5.1-codex",
        PricingStructure::Flat {
            input_per_1m: 1.25,
            output_per_1m: 10.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.125
        },
        false
    );
    add_model!(
        "gpt-5.1-codex-mini",
        PricingStructure::Flat {
            input_per_1m: 0.25,
            output_per_1m: 2.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.025
        },
        false
    );
    add_model!(
        "gpt-5.1-codex-max",
        PricingStructure::Flat {
            input_per_1m: 1.25,
            output_per_1m: 10.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.125
        },
        false
    );
    add_model!(
        "gpt-5.2",
        PricingStructure::Flat {
            input_per_1m: 1.75,
            output_per_1m: 14.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.175
        },
        false
    );
    add_model!(
        "gpt-5.2-pro",
        PricingStructure::Flat {
            input_per_1m: 21.0,
            output_per_1m: 168.0
        },
        CachingSupport::None,
        false
    );
    add_model!(
        "gpt-5.2-codex",
        PricingStructure::Flat {
            input_per_1m: 1.75,
            output_per_1m: 14.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.175
        },
        false
    );
    add_model!(
        "gpt-5.3-codex",
        PricingStructure::Flat {
            input_per_1m: 1.75,
            output_per_1m: 14.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.175
        },
        false
    );
    add_model!(
        "gpt-5-pro",
        PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 120.0
        },
        CachingSupport::None,
        false
    );

    add_model!(
        "gpt-5.4",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 2.50,
                    output_per_1m: 15.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 5.0,
                    output_per_1m: 22.5
                },
            ],
            bracket_pricing: false,
        }),
        CachingSupport::Google(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(272_000),
                    cached_input_per_1m: 0.25
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.50
                },
            ],
            bracket_pricing: false,
        }),
        false
    );

    add_model!(
        "gpt-5.4-pro",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 30.0,
                    output_per_1m: 180.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 60.0,
                    output_per_1m: 270.0
                },
            ],
            bracket_pricing: false,
        }),
        CachingSupport::None,
        false
    );

    add_model!(
        "gpt-5.4-mini",
        PricingStructure::Flat {
            input_per_1m: 0.75,
            output_per_1m: 4.5
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.075
        },
        false
    );
    add_model!(
        "gpt-5.4-nano",
        PricingStructure::Flat {
            input_per_1m: 0.20,
            output_per_1m: 1.25
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.02
        },
        false
    );

    // Anthropic Models
    add_model!(
        "claude-opus-4-7",
        PricingStructure::Flat {
            input_per_1m: 5.0,
            output_per_1m: 25.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 6.25,
            cache_read_per_1m: 0.5
        },
        false
    );
    add_model!(
        "claude-opus-4-6",
        PricingStructure::Flat {
            input_per_1m: 5.0,
            output_per_1m: 25.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 6.25,
            cache_read_per_1m: 0.5
        },
        false
    );
    add_model!(
        "claude-opus-4-5",
        PricingStructure::Flat {
            input_per_1m: 5.0,
            output_per_1m: 25.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 6.25,
            cache_read_per_1m: 0.5
        },
        false
    );
    add_model!(
        "claude-opus-4-1",
        PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 75.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 18.75,
            cache_read_per_1m: 1.5
        },
        false
    );
    add_model!(
        "claude-opus-4",
        PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 75.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 18.75,
            cache_read_per_1m: 1.5
        },
        false
    );
    add_model!(
        "claude-sonnet-4",
        PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 3.75,
            cache_read_per_1m: 0.3
        },
        false
    );
    add_model!(
        "claude-sonnet-4-6",
        PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 3.75,
            cache_read_per_1m: 0.3
        },
        false
    );
    add_model!(
        "claude-sonnet-4-5",
        PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 3.75,
            cache_read_per_1m: 0.3
        },
        false
    );
    add_model!(
        "claude-3-7-sonnet",
        PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 3.75,
            cache_read_per_1m: 0.3
        },
        false
    );
    add_model!(
        "claude-3-5-sonnet",
        PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 3.75,
            cache_read_per_1m: 0.3
        },
        false
    );
    add_model!(
        "claude-3-5-haiku",
        PricingStructure::Flat {
            input_per_1m: 0.8,
            output_per_1m: 4.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 1.0,
            cache_read_per_1m: 0.08
        },
        false
    );
    add_model!(
        "claude-haiku-4-5",
        PricingStructure::Flat {
            input_per_1m: 1.0,
            output_per_1m: 5.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 1.25,
            cache_read_per_1m: 0.10
        },
        false
    );
    add_model!(
        "claude-3-opus",
        PricingStructure::Flat {
            input_per_1m: 15.0,
            output_per_1m: 75.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 18.75,
            cache_read_per_1m: 1.5
        },
        false
    );
    add_model!(
        "claude-3-haiku",
        PricingStructure::Flat {
            input_per_1m: 0.25,
            output_per_1m: 1.25
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 0.3,
            cache_read_per_1m: 0.03
        },
        false
    );

    // Google Models
    add_model!(
        "gemini-3-flash-preview",
        PricingStructure::Flat {
            input_per_1m: 0.5,
            output_per_1m: 3.0
        },
        CachingSupport::Google(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.05
            }],
            bracket_pricing: false,
        }),
        false
    );
    add_model!(
        "gemini-3.1-pro-preview",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 2.0,
                    output_per_1m: 12.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 4.0,
                    output_per_1m: 18.0
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Google(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.20
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.40
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_model!(
        "gemini-3-pro-preview-11-2025",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 2.0,
                    output_per_1m: 12.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 4.0,
                    output_per_1m: 18.0
                },
            ],
            bracket_pricing: false,
        }),
        CachingSupport::None,
        false
    );
    add_model!(
        "gemini-2.5-pro",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 1.25,
                    output_per_1m: 10.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.5,
                    output_per_1m: 15.0
                },
            ],
            bracket_pricing: false,
        }),
        CachingSupport::Google(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.31
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.625
                },
            ],
            bracket_pricing: false,
        }),
        false
    );
    add_model!(
        "gemini-2.5-flash",
        PricingStructure::Flat {
            input_per_1m: 0.3,
            output_per_1m: 2.5
        },
        CachingSupport::Google(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.075
            }],
            bracket_pricing: false,
        }),
        false
    );
    add_model!(
        "gemini-2.5-flash-lite",
        PricingStructure::Flat {
            input_per_1m: 0.1,
            output_per_1m: 0.4
        },
        CachingSupport::Google(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.025
            }],
            bracket_pricing: false,
        }),
        false
    );
    add_model!(
        "gemini-2.0-pro-exp-02-05",
        PricingStructure::Flat {
            input_per_1m: 0.0,
            output_per_1m: 0.0
        },
        CachingSupport::Google(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.0
            }],
            bracket_pricing: false,
        }),
        false
    );
    add_model!(
        "gemini-2.0-flash",
        PricingStructure::Flat {
            input_per_1m: 0.1,
            output_per_1m: 0.4
        },
        CachingSupport::Google(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.025
            }],
            bracket_pricing: false,
        }),
        false
    );
    add_model!(
        "gemini-2.0-flash-lite",
        PricingStructure::Flat {
            input_per_1m: 0.075,
            output_per_1m: 0.3
        },
        CachingSupport::None,
        false
    );
    add_model!(
        "gemini-1.5-flash",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(128_000),
                    input_per_1m: 0.075,
                    output_per_1m: 0.3
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 0.15,
                    output_per_1m: 0.6
                },
            ],
            bracket_pricing: false,
        }),
        CachingSupport::Google(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(128_000),
                    cached_input_per_1m: 0.01875
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.0375
                },
            ],
            bracket_pricing: false,
        }),
        false
    );
    add_model!(
        "gemini-1.5-flash-8b",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(128_000),
                    input_per_1m: 0.0375,
                    output_per_1m: 0.15
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 0.075,
                    output_per_1m: 0.3
                },
            ],
            bracket_pricing: false,
        }),
        CachingSupport::Google(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(128_000),
                    cached_input_per_1m: 0.01
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.02
                },
            ],
            bracket_pricing: false,
        }),
        false
    );
    add_model!(
        "gemini-1.5-pro",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(128_000),
                    input_per_1m: 1.25,
                    output_per_1m: 5.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.5,
                    output_per_1m: 10.0
                },
            ],
            bracket_pricing: false,
        }),
        CachingSupport::Google(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(128_000),
                    cached_input_per_1m: 0.3125
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.625
                },
            ],
            bracket_pricing: false,
        }),
        false
    );

    // Z.AI (Zhipu AI) Models
    add_model!(
        "glm-4.6",
        PricingStructure::Flat {
            input_per_1m: 0.60,
            output_per_1m: 2.20
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.11
        },
        false
    );
    add_model!(
        "glm-4.7",
        PricingStructure::Flat {
            input_per_1m: 0.60,
            output_per_1m: 2.20
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.11
        },
        false
    );
    add_model!(
        "glm-4.7-flash",
        PricingStructure::Flat {
            input_per_1m: 0.0,
            output_per_1m: 0.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.0
        },
        false
    );
    add_model!(
        "glm-4.6v",
        PricingStructure::Flat {
            input_per_1m: 0.30,
            output_per_1m: 0.90
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.05
        },
        false
    );

    // xAI Models
    add_model!(
        "grok-code-fast-1",
        PricingStructure::Flat {
            input_per_1m: 0.20,
            output_per_1m: 1.50
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.02
        },
        false
    );

    // Synthetic.new Models
    add_model!(
        "hf:zai-org/GLM-4.6",
        PricingStructure::Flat {
            input_per_1m: 0.55,
            output_per_1m: 2.19
        },
        CachingSupport::None,
        false
    );
    add_model!(
        "hf:MiniMaxAI/MiniMax-M2",
        PricingStructure::Flat {
            input_per_1m: 0.55,
            output_per_1m: 2.19
        },
        CachingSupport::None,
        false
    );

    // ByteDance / Doubao Models
    add_model!(
        "doubao-seed-2.0-code",
        PricingStructure::Flat {
            input_per_1m: 0.67,
            output_per_1m: 3.36
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.14
        },
        true
    );

    // Z.AI (Zhipu AI) - Additional Models
    add_model!(
        "glm-5",
        PricingStructure::Flat {
            input_per_1m: 1.0,
            output_per_1m: 3.2
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.2
        },
        false
    );
    add_model!(
        "glm-5-code",
        PricingStructure::Flat {
            input_per_1m: 1.2,
            output_per_1m: 5.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.3
        },
        false
    );
    add_model!(
        "glm-4.5-air",
        PricingStructure::Flat {
            input_per_1m: 0.2,
            output_per_1m: 1.1
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.03
        },
        false
    );

    // MiniMax Models
    add_model!(
        "minimax-m2.1",
        PricingStructure::Flat {
            input_per_1m: 0.30,
            output_per_1m: 1.20
        },
        CachingSupport::None,
        false
    );
    add_model!(
        "minimax-m2.5",
        PricingStructure::Flat {
            input_per_1m: 0.30,
            output_per_1m: 1.10
        },
        CachingSupport::None,
        false
    );

    // StepFun Models
    add_model!(
        "step-3.5-flash",
        PricingStructure::Flat {
            input_per_1m: 0.10,
            output_per_1m: 0.30
        },
        CachingSupport::None,
        false
    );

    // Upstage Models
    add_model!(
        "solar-pro-3",
        PricingStructure::Flat {
            input_per_1m: 0.15,
            output_per_1m: 0.60
        },
        CachingSupport::None,
        false
    );

    // OpenRouter Models
    add_model!(
        "aurora-alpha",
        PricingStructure::Flat {
            input_per_1m: 0.0,
            output_per_1m: 0.0
        },
        CachingSupport::None,
        false
    );

    // Populate Aliases
    macro_rules! add_alias {
        ($alias:expr, $canonical:expr) => {
            if $alias != $canonical {
                aliases.insert($alias.to_string(), $canonical.to_string());
            }
        };
    }

    // OpenAI aliases
    add_alias!("o4-mini", "o4-mini");
    add_alias!("o4-mini-2025-04-16", "o4-mini");
    add_alias!("o3", "o3");
    add_alias!("o3-2025-04-16", "o3");
    add_alias!("o3-pro", "o3-pro");
    add_alias!("o3-pro-2025-06-10", "o3-pro");
    add_alias!("o3-mini", "o3-mini");
    add_alias!("o3-mini-2025-01-31", "o3-mini");
    add_alias!("o1", "o1");
    add_alias!("o1-2024-12-17", "o1");
    add_alias!("o1-preview", "o1-preview");
    add_alias!("o1-preview-2024-09-12", "o1-preview");
    add_alias!("o1-mini", "o1-mini");
    add_alias!("o1-mini-2024-09-12", "o1-mini");
    add_alias!("o1-pro", "o1-pro");
    add_alias!("o1-pro-2025-03-19", "o1-pro");
    add_alias!("gpt-4.1", "gpt-4.1");
    add_alias!("gpt-4.1-2025-04-14", "gpt-4.1");
    add_alias!("gpt-4o", "gpt-4o");
    add_alias!("gpt-4o-2024-11-20", "gpt-4o");
    add_alias!("gpt-4o-2024-08-06", "gpt-4o");
    add_alias!("gpt-4o-2024-05-13", "gpt-4o-2024-05-13");
    add_alias!("gpt-4.1-mini", "gpt-4.1-mini");
    add_alias!("gpt-4.1-mini-2025-04-14", "gpt-4.1-mini");
    add_alias!("gpt-4.1-nano", "gpt-4.1-nano");
    add_alias!("gpt-4.1-nano-2025-04-14", "gpt-4.1-nano");
    add_alias!("gpt-4o-mini", "gpt-4o-mini");
    add_alias!("gpt-4o-mini-2024-07-18", "gpt-4o-mini");
    add_alias!("codex-mini-latest", "codex-mini-latest");
    add_alias!("gpt-4-turbo", "gpt-4-turbo");
    add_alias!("gpt-4-turbo-2024-04-09", "gpt-4-turbo");
    add_alias!("gpt-5", "gpt-5");
    add_alias!("gpt-5-codex", "gpt-5");
    add_alias!("gpt-5-2025-08-07", "gpt-5");
    add_alias!("gpt-5.1", "gpt-5.1");
    add_alias!("gpt-5.1-2025-08-07", "gpt-5.1");
    add_alias!("gpt-5-mini", "gpt-5-mini");
    add_alias!("gpt-5-mini-2025-08-07", "gpt-5-mini");
    add_alias!("gpt-5-nano", "gpt-5-nano");
    add_alias!("gpt-5-nano-2025-08-07", "gpt-5-nano");
    add_alias!("gpt-5-codex-mini", "gpt-5-codex-mini");
    add_alias!("gpt-5.1-codex", "gpt-5.1-codex");
    add_alias!("gpt-5.1-codex-mini", "gpt-5.1-codex-mini");
    add_alias!("gpt-5.1-codex-max", "gpt-5.1-codex-max");
    add_alias!("gpt-5.2", "gpt-5.2");
    add_alias!("gpt-5.2-2025-12-11", "gpt-5.2");
    add_alias!("gpt-5.2-pro", "gpt-5.2-pro");
    add_alias!("gpt-5.2-codex", "gpt-5.2-codex");
    add_alias!("gpt-5.3-codex", "gpt-5.3-codex");
    add_alias!("gpt-5-pro", "gpt-5-pro");

    // Anthropic aliases
    add_alias!("claude-opus-4.7", "claude-opus-4-7");
    add_alias!("claude-opus-4.6", "claude-opus-4-6");
    add_alias!("claude-4.6-opus", "claude-opus-4-6");
    add_alias!("claude-4.6-opus-20260205", "claude-opus-4-6");
    add_alias!("claude-opus-4-6", "claude-opus-4-6");
    add_alias!("claude-opus-4-5", "claude-opus-4-5");
    add_alias!("claude-opus-4.5", "claude-opus-4-5");
    add_alias!("claude-opus-4-5-20251101", "claude-opus-4-5");
    add_alias!("claude-opus-4", "claude-opus-4");
    add_alias!("claude-opus-4-20250514", "claude-opus-4");
    add_alias!("claude-opus-4-0", "claude-opus-4");
    add_alias!("claude-opus-4.1", "claude-opus-4-1");
    add_alias!("claude-opus-4-1-20250805", "claude-opus-4-1");
    add_alias!("claude-sonnet-4", "claude-sonnet-4");
    add_alias!("claude-sonnet-4-20250514", "claude-sonnet-4");
    add_alias!("claude-sonnet-4-0", "claude-sonnet-4");
    add_alias!("claude-sonnet-4.6", "claude-sonnet-4-6");
    add_alias!("claude-sonnet-4.5", "claude-sonnet-4-5");
    add_alias!("claude-4.5-sonnet", "claude-sonnet-4-5");
    add_alias!("claude-sonnet-4-5-20250929", "claude-sonnet-4-5");
    add_alias!("claude-3-7-sonnet", "claude-3-7-sonnet");
    add_alias!("claude-3-7-sonnet-20250219", "claude-3-7-sonnet");
    add_alias!("claude-3-7-sonnet-latest", "claude-3-7-sonnet");
    add_alias!("claude-3-5-sonnet", "claude-3-5-sonnet");
    add_alias!("claude-3-5-sonnet-20241022", "claude-3-5-sonnet");
    add_alias!("claude-3-5-sonnet-latest", "claude-3-5-sonnet");
    add_alias!("claude-3-5-sonnet-20240620", "claude-3-5-sonnet");
    add_alias!("claude-3-5-haiku", "claude-3-5-haiku");
    add_alias!("claude-3-5-haiku-20241022", "claude-3-5-haiku");
    add_alias!("claude-3-5-haiku-latest", "claude-3-5-haiku");
    add_alias!("claude-haiku-4-5", "claude-haiku-4-5");
    add_alias!("claude-haiku-4.5", "claude-haiku-4-5");
    add_alias!("claude-haiku-4-5-20251001", "claude-haiku-4-5");
    add_alias!("claude-3-opus", "claude-3-opus");
    add_alias!("claude-3-opus-20240229", "claude-3-opus");
    add_alias!("claude-3-haiku", "claude-3-haiku");
    add_alias!("claude-3-haiku-20240307", "claude-3-haiku");

    // Google aliases
    add_alias!("gemini-3-flash-preview", "gemini-3-flash-preview");
    add_alias!("gemini-3-flash-preview-12-2025", "gemini-3-flash-preview");
    add_alias!("gemini-3-flash", "gemini-3-flash-preview");
    add_alias!("gemini-3.1-pro-preview", "gemini-3.1-pro-preview");
    add_alias!(
        "gemini-3.1-pro-preview-customtools",
        "gemini-3.1-pro-preview"
    );
    add_alias!("gemini-3.1-pro", "gemini-3.1-pro-preview");
    add_alias!("gemini-3.1-pro-low", "gemini-3.1-pro-preview");
    add_alias!("gemini-3.1-pro-medium", "gemini-3.1-pro-preview");
    add_alias!("gemini-3.1-pro-high", "gemini-3.1-pro-preview");
    add_alias!(
        "gemini-3-pro-preview-11-2025",
        "gemini-3-pro-preview-11-2025"
    );
    add_alias!("gemini-3-pro-preview", "gemini-3-pro-preview-11-2025");
    add_alias!("gemini-3-pro", "gemini-3-pro-preview-11-2025");
    add_alias!("gemini-2.5-pro", "gemini-2.5-pro");
    add_alias!("gemini-2.5-pro-preview-06-05", "gemini-2.5-pro");
    add_alias!("gemini-2.5-pro-preview-05-06", "gemini-2.5-pro");
    add_alias!("gemini-2.5-pro-preview-03-25", "gemini-2.5-pro");
    add_alias!("gemini-2.5-flash", "gemini-2.5-flash");
    add_alias!("gemini-2.5-flash-preview-05-20", "gemini-2.5-flash");
    add_alias!("gemini-2.5-flash-preview-04-17", "gemini-2.5-flash");
    add_alias!("gemini-2.5-flash-lite", "gemini-2.5-flash-lite");
    add_alias!("gemini-2.5-flash-lite-06-17", "gemini-2.5-flash-lite");
    add_alias!("gemini-2.0-pro-exp-02-05", "gemini-2.0-pro-exp-02-05");
    add_alias!("gemini-exp-1206", "gemini-2.0-pro-exp-02-05");
    add_alias!("gemini-2.0-flash", "gemini-2.0-flash");
    add_alias!("gemini-2.0-flash-001", "gemini-2.0-flash");
    add_alias!("gemini-2.0-flash-exp", "gemini-2.0-flash");
    add_alias!("gemini-2.0-flash-lite", "gemini-2.0-flash-lite");
    add_alias!("gemini-2.0-flash-lite-001", "gemini-2.0-flash-lite");
    add_alias!("gemini-1.5-flash", "gemini-1.5-flash");
    add_alias!("gemini-1.5-flash-latest", "gemini-1.5-flash");
    add_alias!("gemini-1.5-flash-001", "gemini-1.5-flash");
    add_alias!("gemini-1.5-flash-002", "gemini-1.5-flash");
    add_alias!("gemini-1.5-flash-8b", "gemini-1.5-flash-8b");
    add_alias!("gemini-1.5-flash-8b-latest", "gemini-1.5-flash-8b");
    add_alias!("gemini-1.5-flash-8b-001", "gemini-1.5-flash-8b");
    add_alias!("gemini-1.5-flash-8b-exp-0924", "gemini-1.5-flash-8b");
    add_alias!("gemini-1.5-flash-8b-exp-0827", "gemini-1.5-flash-8b");
    add_alias!("gemini-1.5-pro", "gemini-1.5-pro");
    add_alias!("gemini-1.5-pro-latest", "gemini-1.5-pro");
    add_alias!("gemini-1.5-pro-001", "gemini-1.5-pro");
    add_alias!("gemini-1.5-pro-002", "gemini-1.5-pro");
    add_alias!("gemini-1.5-pro-exp-0827", "gemini-1.5-pro");
    add_alias!("gemini-1.5-pro-exp-0801", "gemini-1.5-pro");

    // Zhipu AI aliases
    add_alias!("zai-glm-4.6", "glm-4.6");
    add_alias!("glm-5-20260211", "glm-5");
    add_alias!("glm-5-code", "glm-5-code");
    add_alias!("glm-5-code-20260211", "glm-5-code");
    add_alias!("glm-4.5-air-20260211", "glm-4.5-air");

    // OpenAI aliases (continued)
    add_alias!("gpt-5.4", "gpt-5.4");
    add_alias!("gpt-5.4-2026-03-05", "gpt-5.4");
    add_alias!("gpt-5.4-pro", "gpt-5.4-pro");
    add_alias!("gpt-5.4-mini", "gpt-5.4-mini");
    add_alias!("gpt-5.4-mini-2026-03-17", "gpt-5.4-mini");
    add_alias!("gpt-5.4-mini-2026-03-17.", "gpt-5.4-mini");

    // MiniMax aliases
    add_alias!("minimax-m2.1", "minimax-m2.1");
    add_alias!("minimax-m2.5", "minimax-m2.5");
    add_alias!("minimax-m2.5-20260211", "minimax-m2.5");

    // Moonshot / ByteDance / Meituan aliases
    add_alias!("doubao-seed-code", "doubao-seed-2.0-code");

    // StepFun aliases
    add_alias!("step-3.5-flash", "step-3.5-flash");

    // Upstage aliases
    add_alias!("solar-pro-3", "solar-pro-3");

    // Aurora aliases
    add_alias!("aurora-alpha", "aurora-alpha");
}

/// Free-tier model pricing for models accessed via OpenRouter's `:free` suffix
/// or other free-tier naming patterns.
fn get_free_model_info() -> Arc<ModelInfo> {
    Arc::clone(FREE_MODEL_INFO.get_or_init(|| {
        Arc::new(ModelInfo {
            pricing: PricingStructure::Flat {
                input_per_1m: 0.0,
                output_per_1m: 0.0,
            },
            caching: CachingSupport::None,
            is_estimated: false,
        })
    }))
}

/// Look up a model name directly in the index and alias tables.
fn lookup_model(name: &str) -> Option<Arc<ModelInfo>> {
    let registry = get_registry_lock().read();
    let mut current = name;
    let mut visited = HashSet::new();

    loop {
        if let Some(model_info) = registry.index.get(current) {
            return Some(Arc::clone(model_info));
        }
        if !visited.insert(current.to_string()) {
            return None;
        }
        current = registry.aliases.get(current)?.as_str();
    }
}

/// Get model info by any valid name (canonical or alias).
///
/// Handles provider-prefixed model names (e.g. `minimax/minimax-m2.5`,
/// `z-ai/glm-5`, `openrouter/aurora-alpha`) by stripping the prefix before
/// lookup. Models with a `:free` suffix (OpenRouter free tier) always
/// return $0 pricing.
pub fn get_model_info(model_name: &str) -> Option<Arc<ModelInfo>> {
    // Fast path: direct lookup
    if let Some(info) = lookup_model(model_name) {
        return Some(info);
    }

    // Normalize: strip provider prefix (everything before last `/`)
    let after_slash = model_name
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(model_name);

    // Handle `:free` suffix → always $0
    if after_slash.strip_suffix(":free").is_some() {
        return Some(get_free_model_info());
    }

    // Handle other suffixes like `:extended`
    let base_name = after_slash.strip_suffix(":extended").unwrap_or(after_slash);

    // Try the normalized name (only if different from original)
    if base_name != model_name
        && let Some(info) = lookup_model(base_name)
    {
        return Some(info);
    }

    // Also handle patterns like "minimax-m2.5-free" (without colon)
    if base_name.strip_suffix("-free").is_some() {
        return Some(get_free_model_info());
    }

    None
}

/// Check if a model's pricing is estimated (not officially published)
pub fn is_model_estimated(model_name: &str) -> bool {
    get_model_info(model_name)
        .map(|info| info.is_estimated)
        .unwrap_or(false)
}

fn input_cost_for_model(model_info: &ModelInfo, input_tokens: u64) -> f64 {
    match &model_info.pricing {
        PricingStructure::Flat { input_per_1m, .. } => {
            (input_tokens as f64 / 1_000_000.0) * input_per_1m
        }
        PricingStructure::Tiered(tiered) => {
            calculate_tiered_cost(input_tokens, &tiered.tiers, tiered.bracket_pricing, true)
        }
    }
}

/// Calculate cost for input tokens using the model's pricing structure
pub fn calculate_input_cost(model_name: &str, input_tokens: u64) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => input_cost_for_model(&model_info, input_tokens),
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            (input_tokens as f64 / 1_000_000.0) * 0.0 // $0 per 1M tokens fallback
        }
    }
}

fn output_cost_for_model(model_info: &ModelInfo, output_tokens: u64) -> f64 {
    match &model_info.pricing {
        PricingStructure::Flat { output_per_1m, .. } => {
            (output_tokens as f64 / 1_000_000.0) * output_per_1m
        }
        PricingStructure::Tiered(tiered) => {
            calculate_tiered_cost(output_tokens, &tiered.tiers, tiered.bracket_pricing, false)
        }
    }
}

/// Calculate cost for output tokens using the model's pricing structure
pub fn calculate_output_cost(model_name: &str, output_tokens: u64) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => output_cost_for_model(&model_info, output_tokens),
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            (output_tokens as f64 / 1_000_000.0) * 0.0 // $0 per 1M tokens fallback
        }
    }
}

fn cache_cost_for_model(
    model_info: &ModelInfo,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
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
            let creation_cost = (cache_creation_tokens as f64 / 1_000_000.0) * cache_write_per_1m;
            let read_cost = (cache_read_tokens as f64 / 1_000_000.0) * cache_read_per_1m;
            creation_cost + read_cost
        }
        CachingSupport::Google(tiered) => {
            calculate_tiered_cache_cost(cache_read_tokens, &tiered.tiers, tiered.bracket_pricing)
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
            cache_cost_for_model(&model_info, cache_creation_tokens, cache_read_tokens)
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
    match get_model_info(model_name) {
        Some(model_info) => {
            input_cost_for_model(&model_info, input_tokens)
                + output_cost_for_model(&model_info, output_tokens)
                + cache_cost_for_model(&model_info, cache_creation_tokens, cache_read_tokens)
        }
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            0.0
        }
    }
}

fn calculate_tiered_cost(
    tokens: u64,
    tiers: &[PricingTier],
    bracket_pricing: bool,
    is_input: bool,
) -> f64 {
    if bracket_pricing {
        if let Some(tier) = find_tier(tokens, tiers, |tier| tier.max_tokens) {
            let rate = if is_input {
                tier.input_per_1m
            } else {
                tier.output_per_1m
            };

            return (tokens as f64 / 1_000_000.0) * rate;
        }

        return 0.0;
    }

    let mut total_cost = 0.0;
    let mut remaining_tokens = tokens;
    let mut lower_bound = 0;

    for tier in tiers {
        if remaining_tokens == 0 {
            break;
        }

        let upper_bound = tier.max_tokens.unwrap_or(u64::MAX);
        let tier_width = upper_bound.saturating_sub(lower_bound);
        let tokens_in_tier = remaining_tokens.min(tier_width);

        let rate = if is_input {
            tier.input_per_1m
        } else {
            tier.output_per_1m
        };
        total_cost += (tokens_in_tier as f64 / 1_000_000.0) * rate;

        remaining_tokens = remaining_tokens.saturating_sub(tokens_in_tier);
        lower_bound = upper_bound;
    }

    total_cost
}

fn calculate_tiered_cache_cost(tokens: u64, tiers: &[CachingTier], bracket_pricing: bool) -> f64 {
    if bracket_pricing {
        if let Some(tier) = find_tier(tokens, tiers, |tier| tier.max_tokens) {
            return (tokens as f64 / 1_000_000.0) * tier.cached_input_per_1m;
        }

        return 0.0;
    }

    let mut total_cost = 0.0;
    let mut remaining_tokens = tokens;
    let mut lower_bound = 0;

    for tier in tiers {
        if remaining_tokens == 0 {
            break;
        }

        let upper_bound = tier.max_tokens.unwrap_or(u64::MAX);
        let tier_width = upper_bound.saturating_sub(lower_bound);
        let tokens_in_tier = remaining_tokens.min(tier_width);

        total_cost += (tokens_in_tier as f64 / 1_000_000.0) * tier.cached_input_per_1m;

        remaining_tokens = remaining_tokens.saturating_sub(tokens_in_tier);
        lower_bound = upper_bound;
    }

    total_cost
}

fn find_tier<T, F>(tokens: u64, tiers: &[T], max_tokens: F) -> Option<&T>
where
    F: Fn(&T) -> Option<u64>,
{
    for tier in tiers {
        match max_tokens(tier) {
            Some(limit) if tokens <= limit => return Some(tier),
            None => return Some(tier),
            _ => continue,
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{
        CachingSupport, CachingTier, ModelInfo, PricingStructure, PricingTier, Registry,
        TieredCaching, TieredPricing, calculate_cache_cost, calculate_input_cost,
        calculate_output_cost, get_model_info, get_registry_lock, init_external_models,
    };

    use std::collections::HashMap;

    fn approx_eq(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-9, "left={left}, right={right}");
    }

    fn reset_global_registry() {
        let registry = get_registry_lock();
        *registry.write() = Registry::new_with_defaults();
    }

    #[test]
    fn test_registry_merging() {
        let mut registry = Registry::new_with_defaults();
        let mut custom_models = HashMap::new();
        custom_models.insert(
            "super-expensive-o3".to_string(),
            ModelInfo {
                pricing: PricingStructure::Flat {
                    input_per_1m: 1000.0,
                    output_per_1m: 2000.0,
                },
                caching: CachingSupport::None,
                is_estimated: false,
            },
        );

        let mut custom_aliases = HashMap::new();
        custom_aliases.insert("expensive".to_string(), "super-expensive-o3".to_string());

        registry.merge(custom_models, custom_aliases);

        let info = registry
            .index
            .get("super-expensive-o3")
            .expect("Should find custom model");
        match &info.pricing {
            PricingStructure::Flat { input_per_1m, .. } => assert_eq!(*input_per_1m, 1000.0),
            _ => panic!("Expected flat pricing"),
        }

        let canonical = registry
            .aliases
            .get("expensive")
            .expect("Should find aliased model");
        assert_eq!(canonical, "super-expensive-o3");
    }

    #[test]
    fn init_external_models_accepts_multiple_calls() {
        reset_global_registry();

        let mut first_models = HashMap::new();
        first_models.insert(
            "review-first-model".to_string(),
            ModelInfo {
                pricing: PricingStructure::Flat {
                    input_per_1m: 1.0,
                    output_per_1m: 2.0,
                },
                caching: CachingSupport::None,
                is_estimated: false,
            },
        );
        let mut first_aliases = HashMap::new();
        first_aliases.insert(
            "review-first-alias".to_string(),
            "review-first-model".to_string(),
        );

        init_external_models(first_models, first_aliases);
        assert!(get_model_info("review-first-alias").is_some());

        let mut second_models = HashMap::new();
        second_models.insert(
            "review-second-model".to_string(),
            ModelInfo {
                pricing: PricingStructure::Flat {
                    input_per_1m: 3.0,
                    output_per_1m: 4.0,
                },
                caching: CachingSupport::None,
                is_estimated: false,
            },
        );
        let mut second_aliases = HashMap::new();
        second_aliases.insert(
            "review-second-alias".to_string(),
            "review-second-model".to_string(),
        );

        init_external_models(second_models, second_aliases);

        assert!(get_model_info("review-second-alias").is_some());

        reset_global_registry();
    }

    #[test]
    fn transitive_aliases_resolve_to_the_final_model() {
        reset_global_registry();

        let mut models = HashMap::new();
        models.insert(
            "review-chain-model".to_string(),
            ModelInfo {
                pricing: PricingStructure::Flat {
                    input_per_1m: 1.5,
                    output_per_1m: 2.5,
                },
                caching: CachingSupport::None,
                is_estimated: false,
            },
        );

        let mut aliases = HashMap::new();
        aliases.insert("review-chain-a".to_string(), "review-chain-b".to_string());
        aliases.insert(
            "review-chain-b".to_string(),
            "review-chain-model".to_string(),
        );

        init_external_models(models, aliases);

        let model_info = get_model_info("review-chain-a").expect("alias chain should resolve");
        match &model_info.pricing {
            PricingStructure::Flat { input_per_1m, .. } => approx_eq(*input_per_1m, 1.5),
            _ => panic!("Expected flat pricing"),
        }

        reset_global_registry();
    }

    #[test]
    fn invalid_external_tier_configs_are_skipped() {
        reset_global_registry();

        let mut models = HashMap::new();
        models.insert(
            "review-invalid-tier-model".to_string(),
            ModelInfo {
                pricing: PricingStructure::Tiered(TieredPricing {
                    tiers: vec![
                        PricingTier {
                            max_tokens: Some(200),
                            input_per_1m: 1.0,
                            output_per_1m: 2.0,
                        },
                        PricingTier {
                            max_tokens: Some(100),
                            input_per_1m: 3.0,
                            output_per_1m: 4.0,
                        },
                    ],
                    bracket_pricing: false,
                }),
                caching: CachingSupport::Google(TieredCaching {
                    tiers: vec![
                        CachingTier {
                            max_tokens: Some(50),
                            cached_input_per_1m: 0.5,
                        },
                        CachingTier {
                            max_tokens: Some(25),
                            cached_input_per_1m: 0.25,
                        },
                    ],
                    bracket_pricing: false,
                }),
                is_estimated: false,
            },
        );

        let mut aliases = HashMap::new();
        aliases.insert(
            "review-invalid-tier-alias".to_string(),
            "review-invalid-tier-model".to_string(),
        );

        init_external_models(models, aliases);

        assert!(get_model_info("review-invalid-tier-model").is_none());
        assert!(get_model_info("review-invalid-tier-alias").is_none());

        reset_global_registry();
    }

    #[test]
    fn gemini_3_1_pro_preview_uses_bracket_pricing_for_input() {
        let cost = calculate_input_cost("gemini-3.1-pro-preview", 250_000);
        approx_eq(cost, 1.0);
    }

    #[test]
    fn gemini_3_1_pro_preview_uses_bracket_pricing_for_cache_reads() {
        let cost = calculate_cache_cost("gemini-3.1-pro-preview", 0, 250_000);
        approx_eq(cost, 0.1);
    }

    #[test]
    fn gemini_3_1_pro_preview_customtools_alias_maps_to_same_pricing() {
        let model_info =
            get_model_info("gemini-3.1-pro-preview-customtools").expect("alias should resolve");
        assert!(!model_info.is_estimated);

        let input_cost = calculate_input_cost("gemini-3.1-pro-preview-customtools", 250_000);
        let output_cost = calculate_output_cost("gemini-3.1-pro-preview-customtools", 250_000);
        let cache_cost = calculate_cache_cost("gemini-3.1-pro-preview-customtools", 0, 250_000);

        approx_eq(input_cost, 1.0);
        approx_eq(output_cost, 4.5);
        approx_eq(cache_cost, 0.1);
    }

    #[test]
    fn gemini_2_5_pro_remains_progressive() {
        let cost = calculate_input_cost("gemini-2.5-pro", 250_000);
        approx_eq(cost, 0.375);
    }

    #[test]
    fn gpt_5_4_mini_alias_maps_to_pricing() {
        let model_info = get_model_info("gpt-5.4-mini-2026-03-17.").expect("model should exist");
        assert!(!model_info.is_estimated);

        let input_cost = calculate_input_cost("gpt-5.4-mini-2026-03-17.", 1_000_000);
        let output_cost = calculate_output_cost("gpt-5.4-mini-2026-03-17.", 1_000_000);
        let cache_cost = calculate_cache_cost("gpt-5.4-mini-2026-03-17.", 0, 1_000_000);

        approx_eq(input_cost, 0.75);
        approx_eq(output_cost, 4.5);
        approx_eq(cache_cost, 0.075);
    }

    #[test]
    fn gpt_5_4_nano_pricing_is_available() {
        let model_info = get_model_info("gpt-5.4-nano").expect("model should exist");
        assert!(!model_info.is_estimated);

        let input_cost = calculate_input_cost("gpt-5.4-nano", 1_000_000);
        let output_cost = calculate_output_cost("gpt-5.4-nano", 1_000_000);
        let cache_cost = calculate_cache_cost("gpt-5.4-nano", 0, 1_000_000);

        approx_eq(input_cost, 0.20);
        approx_eq(output_cost, 1.25);
        approx_eq(cache_cost, 0.02);
    }

    #[test]
    fn gpt_4_5_pricing_is_available() {
        let model_info = get_model_info("gpt-4.5").expect("model should exist");
        assert!(!model_info.is_estimated);

        let input_cost = calculate_input_cost("gpt-4.5", 1_000_000);
        let output_cost = calculate_output_cost("gpt-4.5", 1_000_000);
        let cache_cost = calculate_cache_cost("gpt-4.5", 0, 1_000_000);

        approx_eq(input_cost, 75.0);
        approx_eq(output_cost, 150.0);
        approx_eq(cache_cost, 37.5);
    }

    #[test]
    fn doubao_seed_code_alias_resolves() {
        let model_info = get_model_info("doubao-seed-code").expect("alias should resolve");
        assert!(model_info.is_estimated);

        let input_cost = calculate_input_cost("doubao-seed-code", 1_000_000);
        let output_cost = calculate_output_cost("doubao-seed-code", 1_000_000);
        let cache_cost = calculate_cache_cost("doubao-seed-code", 0, 1_000_000);

        approx_eq(input_cost, 0.67);
        approx_eq(output_cost, 3.36);
        approx_eq(cache_cost, 0.14);
    }

    #[test]
    fn repeated_tiered_model_lookups_reuse_the_same_tier_storage() {
        let first = get_model_info("gemini-2.5-pro").expect("model should exist");
        let second = get_model_info("gemini-2.5-pro").expect("model should exist");

        match (&first.pricing, &second.pricing) {
            (PricingStructure::Tiered(first_tiered), PricingStructure::Tiered(second_tiered)) => {
                assert!(
                    std::ptr::eq(first_tiered.tiers.as_ptr(), second_tiered.tiers.as_ptr()),
                    "tier pricing should not be reallocated on each lookup"
                );
            }
            _ => panic!("Expected tiered pricing"),
        }

        match (&first.caching, &second.caching) {
            (CachingSupport::Google(first_tiered), CachingSupport::Google(second_tiered)) => {
                assert!(
                    std::ptr::eq(first_tiered.tiers.as_ptr(), second_tiered.tiers.as_ptr()),
                    "cache tiers should not be reallocated on each lookup"
                );
            }
            _ => panic!("Expected Google caching"),
        }
    }
}
