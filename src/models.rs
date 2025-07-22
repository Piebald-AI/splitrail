use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::ModelPricing;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ModelSpecificRules {
    None,
    OpenAI {
        // OpenAI only has cached input pricing, no cache creation cost
        cached_input_only: bool,
    },
    Gemini {
        high_volume_input_cost_per_token: Option<f64>,
        high_volume_output_cost_per_token: Option<f64>,
        high_volume_threshold: u64, // Default to 200k tokens
        // Gemini's context caching pricing (per 1M tokens)
        context_caching_less_200k: Option<f64>,
        context_caching_greater_200k: Option<f64>,
    },
}

lazy_static::lazy_static! {
    pub static ref MODEL_PRICING: HashMap<String, ModelPricing> = {
        let mut m = HashMap::new();

        // Claude.
        m.insert("claude-sonnet-4-20250514".to_string(), ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_creation_input_token_cost: 0.00000375,
            cache_read_input_token_cost: 0.0000003,
            model_rules: ModelSpecificRules::None,
        });
        m.insert("claude-opus-4-20250514".to_string(), ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            cache_creation_input_token_cost: 0.00001875,
            cache_read_input_token_cost: 0.0000015,
            model_rules: ModelSpecificRules::None,
        });

        // OpenAI.

        // GPT-4.1 series
        m.insert("gpt-4.1-2025-04-14".to_string(), ModelPricing {
            input_cost_per_token: 0.000002,      // $2.00.
            output_cost_per_token: 0.000008,     // $8.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000005, // $0.50.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4.1-mini-2025-04-14".to_string(), ModelPricing {
            input_cost_per_token: 0.0000004,     // $0.40.
            output_cost_per_token: 0.0000016,    // $1.60.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000001, // $0.10.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4.1-nano-2025-04-14".to_string(), ModelPricing {
            input_cost_per_token: 0.0000001,     // $0.10.
            output_cost_per_token: 0.0000004,    // $0.40.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.000000025, // $0.025.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4.5-preview-2025-02-27".to_string(), ModelPricing {
            input_cost_per_token: 0.000075,      // $75.00.
            output_cost_per_token: 0.00015,      // $150.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000375, // $37.50.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });

        // GPT-4o series
        m.insert("gpt-4o-2024-08-06".to_string(), ModelPricing {
            input_cost_per_token: 0.0000025,     // $2.50.
            output_cost_per_token: 0.00001,      // $10.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.00000125, // $1.25.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4o".to_string(), ModelPricing {
            input_cost_per_token: 0.0000025, // $2.50.
            output_cost_per_token: 0.00001, // $10.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.00000125, // $1.25.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4o-audio-preview-2024-12-17".to_string(), ModelPricing {
            input_cost_per_token: 0.0000025,     // $2.50.
            output_cost_per_token: 0.00001,      // $10.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000025, // No specific cached rate, using input rate.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4o-realtime-preview-2025-06-03".to_string(), ModelPricing {
            input_cost_per_token: 0.000005,      // $5.00.
            output_cost_per_token: 0.00002,      // $20.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000025, // $2.50.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4o-mini-2024-07-18".to_string(), ModelPricing {
            input_cost_per_token: 0.00000015,    // $0.15.
            output_cost_per_token: 0.0000006,    // $0.60.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.000000075, // $0.075.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4o-mini".to_string(), ModelPricing {
            input_cost_per_token: 0.00000015,
            output_cost_per_token: 0.0000006,
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.000000075, // $0.075.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4o-mini-audio-preview-2024-12-17".to_string(), ModelPricing {
            input_cost_per_token: 0.00000015,    // $0.15.
            output_cost_per_token: 0.0000006,    // $0.60.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.00000015, // No specific cached rate, using input rate.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4o-mini-realtime-preview-2024-12-17".to_string(), ModelPricing {
            input_cost_per_token: 0.0000006,     // $0.60.
            output_cost_per_token: 0.0000024,    // $2.40.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000003, // $0.30.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4o-search-preview-2025-03-11".to_string(), ModelPricing {
            input_cost_per_token: 0.0000025,     // $2.50.
            output_cost_per_token: 0.00001,      // $10.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000025, // No specific cached rate, using input rate.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("gpt-4o-mini-search-preview-2025-03-11".to_string(), ModelPricing {
            input_cost_per_token: 0.00000015,    // $0.15.
            output_cost_per_token: 0.0000006,    // $0.60.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.00000015, // No specific cached rate, using input rate.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });

        // o1 series
        m.insert("o1-2024-12-17".to_string(), ModelPricing {
            input_cost_per_token: 0.000015,      // $15.00.
            output_cost_per_token: 0.00006,      // $60.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000075, // $7.50.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("o1".to_string(), ModelPricing {
            input_cost_per_token: 0.000015,  // $15.00.
            output_cost_per_token: 0.00006,  // $60.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000075, // $7.50.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("o1-pro-2025-03-19".to_string(), ModelPricing {
            input_cost_per_token: 0.00015,       // $150.00.
            output_cost_per_token: 0.0006,       // $600.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.00015, // No specific cached rate, using input rate.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("o1-mini-2024-09-12".to_string(), ModelPricing {
            input_cost_per_token: 0.0000011,     // $1.10.
            output_cost_per_token: 0.0000044,    // $4.40.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.00000055, // $0.55.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("o1-mini".to_string(), ModelPricing {
            input_cost_per_token: 0.0000011,   // $1.10.
            output_cost_per_token: 0.0000044,  // $4.40.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.00000055, // $0.55.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });

        // o3 series
        m.insert("o3-pro-2025-06-10".to_string(), ModelPricing {
            input_cost_per_token: 0.00002,       // $20.00.
            output_cost_per_token: 0.00008,      // $80.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.00002, // No specific cached rate, using input rate.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("o3-2025-04-16".to_string(), ModelPricing {
            input_cost_per_token: 0.000002,      // $2.00.
            output_cost_per_token: 0.000008,     // $8.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000005, // $0.50.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("o3-deep-research-2025-06-26".to_string(), ModelPricing {
            input_cost_per_token: 0.00001,       // $10.00.
            output_cost_per_token: 0.00004,      // $40.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000025, // $2.50.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("o3-mini-2025-01-31".to_string(), ModelPricing {
            input_cost_per_token: 0.0000011,     // $1.10.
            output_cost_per_token: 0.0000044,    // $4.40.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.00000055, // $0.55.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });

        // o4 series
        m.insert("o4-mini-2025-04-16".to_string(), ModelPricing {
            input_cost_per_token: 0.0000011,     // $1.10.
            output_cost_per_token: 0.0000044,    // $4.40.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.000000275, // $0.275.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });
        m.insert("o4-mini-deep-research-2025-06-26".to_string(), ModelPricing {
            input_cost_per_token: 0.000002,      // $2.00.
            output_cost_per_token: 0.000008,     // $8.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.0000005, // $0.50.
            model_rules: ModelSpecificRules::OpenAI { cached_input_only: true },
        });

        // Codex models
        m.insert("codex-mini-latest".to_string(), ModelPricing {
            input_cost_per_token: 0.0000015,     // $1.50.
            output_cost_per_token: 0.000006,     // $6.00.
            cache_creation_input_token_cost: 0.0, // Outwardly, OpenAI doesn't do cache creation.
            cache_read_input_token_cost: 0.000000375, // $0.375.
            model_rules: ModelSpecificRules::None,
        });

        // Gemini models - Updated pricing from provided JSON data
        m.insert("gemini-2.5-pro".to_string(), ModelPricing {
            input_cost_per_token: 0.00000125,    // $1.25 per 1M
            output_cost_per_token: 0.000003,     // $3.00 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.00000125),  // $1.25 per 1M for >200k
                high_volume_output_cost_per_token: Some(0.000003),   // $3.00 per 1M for >200k
                high_volume_threshold: 200_000,
                context_caching_less_200k: Some(0.00000031),         // $0.31 per 1M for <200k
                context_caching_greater_200k: Some(0.000000625),     // $0.625 per 1M for >200k
            },
        });

        m.insert("gemini-2.5-flash".to_string(), ModelPricing {
            input_cost_per_token: 0.0000003,     // $0.30 per 1M
            output_cost_per_token: 0.0000015,    // $1.50 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.0000003),   // Same price for all volumes
                high_volume_output_cost_per_token: Some(0.0000015),  // Same price for all volumes
                high_volume_threshold: 200_000,
                context_caching_less_200k: Some(0.000000075),        // $0.075 per 1M
                context_caching_greater_200k: Some(0.000000075),     // $0.075 per 1M (same)
            },
        });

        m.insert("gemini-2.5-flash-lite-preview".to_string(), ModelPricing {
            input_cost_per_token: 0.000000075,   // $0.075 per 1M
            output_cost_per_token: 0.0000003,    // $0.30 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.000000075), // Same price for all volumes
                high_volume_output_cost_per_token: Some(0.0000003),  // Same price for all volumes
                high_volume_threshold: 200_000,
                context_caching_less_200k: Some(0.000000025),        // $0.025 per 1M
                context_caching_greater_200k: Some(0.000000025),     // $0.025 per 1M (same)
            },
        });

        m.insert("gemini-2.5-flash-native-audio".to_string(), ModelPricing {
            input_cost_per_token: 0.0000005,     // $0.50 per 1M
            output_cost_per_token: 0.000002,     // $2.00 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.0000005),  // Same pricing - no tiered pricing in JSON
                high_volume_output_cost_per_token: Some(0.000002),  // Same pricing - no tiered pricing in JSON
                high_volume_threshold: 200_000,
                context_caching_less_200k: None,     // No context caching for this model
                context_caching_greater_200k: None,  // No context caching for this model
            },
        });

        m.insert("gemini-2.5-flash-preview-tts".to_string(), ModelPricing {
            input_cost_per_token: 0.0000005,     // $0.50 per 1M
            output_cost_per_token: 0.00001,      // $10.00 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.0000005),  // Same pricing - no tiered pricing in JSON
                high_volume_output_cost_per_token: Some(0.00001),   // Same pricing - no tiered pricing in JSON
                high_volume_threshold: 200_000,
                context_caching_less_200k: None,     // No context caching for this model
                context_caching_greater_200k: None,  // No context caching for this model
            },
        });

        m.insert("gemini-2.5-pro-preview-tts".to_string(), ModelPricing {
            input_cost_per_token: 0.000001,      // $1.00 per 1M
            output_cost_per_token: 0.00002,      // $20.00 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.000001),   // Same pricing - no tiered pricing in JSON
                high_volume_output_cost_per_token: Some(0.00002),   // Same pricing - no tiered pricing in JSON
                high_volume_threshold: 200_000,
                context_caching_less_200k: None,     // No context caching for this model
                context_caching_greater_200k: None,  // No context caching for this model
            },
        });

        m.insert("gemini-2.0-flash".to_string(), ModelPricing {
            input_cost_per_token: 0.000000075,   // $0.075 per 1M
            output_cost_per_token: 0.0000003,    // $0.30 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.000000075), // Same pricing for all volumes
                high_volume_output_cost_per_token: Some(0.0000003),  // Same pricing for all volumes
                high_volume_threshold: 200_000,
                context_caching_less_200k: Some(0.000000025),        // $0.025 per 1M
                context_caching_greater_200k: Some(0.000000025),     // $0.025 per 1M (same)
            },
        });

        m.insert("gemini-2.0-flash-lite".to_string(), ModelPricing {
            input_cost_per_token: 0.000000075,   // $0.075 per 1M
            output_cost_per_token: 0.0000003,    // $0.30 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.000000075), // Same pricing for all volumes
                high_volume_output_cost_per_token: Some(0.0000003),  // Same pricing for all volumes
                high_volume_threshold: 200_000,
                context_caching_less_200k: None,     // No context caching for this model
                context_caching_greater_200k: None,  // No context caching for this model
            },
        });

        // Free Google models.  Gemini 1 and Gemma.
        m.insert("gemma-3".to_string(), ModelPricing {
            input_cost_per_token: 0.0,           // Free
            output_cost_per_token: 0.0,          // Free
            cache_creation_input_token_cost: 0.0,
            cache_read_input_token_cost: 0.0,
            model_rules: ModelSpecificRules::None,
        });

        m.insert("gemma-3n".to_string(), ModelPricing {
            input_cost_per_token: 0.0,           // Free
            output_cost_per_token: 0.0,          // Free
            cache_creation_input_token_cost: 0.0,
            cache_read_input_token_cost: 0.0,
            model_rules: ModelSpecificRules::None,
        });

        // Older Gemini models.

        m.insert("gemini-1.5-flash".to_string(), ModelPricing {
            input_cost_per_token: 0.000000075,   // $0.075 per 1M
            output_cost_per_token: 0.0000003,    // $0.30 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.00000015), // $0.15 per 1M for >200k
                high_volume_output_cost_per_token: Some(0.0000006), // $0.60 per 1M for >200k
                high_volume_threshold: 200_000,
                context_caching_less_200k: Some(0.00000001875),     // $0.01875 per 1M for <200k
                context_caching_greater_200k: Some(0.0000000375),   // $0.0375 per 1M for >200k
            },
        });

        m.insert("gemini-1.5-flash-8b".to_string(), ModelPricing {
            input_cost_per_token: 0.0000000375,  // $0.0375 per 1M
            output_cost_per_token: 0.00000015,   // $0.15 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.000000075), // $0.075 per 1M for >200k
                high_volume_output_cost_per_token: Some(0.0000003),  // $0.30 per 1M for >200k
                high_volume_threshold: 200_000,
                context_caching_less_200k: Some(0.00000001),         // $0.01 per 1M for <200k
                context_caching_greater_200k: Some(0.00000002),      // $0.02 per 1M for >200k
            },
        });

        m.insert("gemini-1.5-pro".to_string(), ModelPricing {
            input_cost_per_token: 0.00000125,    // $1.25 per 1M
            output_cost_per_token: 0.000005,     // $5.00 per 1M
            cache_creation_input_token_cost: 0.0, // Not applicable for Gemini
            cache_read_input_token_cost: 0.0,     // Not applicable for Gemini
            model_rules: ModelSpecificRules::Gemini {
                high_volume_input_cost_per_token: Some(0.0000025),  // $2.5 per 1M for >200k
                high_volume_output_cost_per_token: Some(0.00001),   // $10.0 per 1M for >200k
                high_volume_threshold: 200_000,
                context_caching_less_200k: Some(0.0000003125),      // $0.3125 per 1M for <200k
                context_caching_greater_200k: Some(0.000000625),    // $0.625 per 1M for >200k
            },
        });

        m
    };
}
