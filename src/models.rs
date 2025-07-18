use std::collections::HashMap;

use crate::types::ModelPricing;

// Hard-coded model info for now.
lazy_static::lazy_static! {
    pub static ref MODEL_PRICING: HashMap<String, ModelPricing> = {
        let mut m = HashMap::new();
        
        // Claude models
        m.insert("claude-sonnet-4-20250514".to_string(), ModelPricing {
            input_cost_per_token: 0.000003,
            output_cost_per_token: 0.000015,
            cache_creation_input_token_cost: 0.00000375,
            cache_read_input_token_cost: 0.0000003,
        });
        m.insert("claude-opus-4-20250514".to_string(), ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.000075,
            cache_creation_input_token_cost: 0.00001875,
            cache_read_input_token_cost: 0.0000015,
        });
        
        // OpenAI models (pricing per 1M tokens converted to per token)
        
        // GPT-4.1 series
        m.insert("gpt-4.1-2025-04-14".to_string(), ModelPricing {
            input_cost_per_token: 0.000002,      // $2.00 per 1M
            output_cost_per_token: 0.000008,     // $8.00 per 1M
            cache_creation_input_token_cost: 0.000002,
            cache_read_input_token_cost: 0.0000005, // $0.50 per 1M
        });
        m.insert("gpt-4.1-mini-2025-04-14".to_string(), ModelPricing {
            input_cost_per_token: 0.0000004,     // $0.40 per 1M
            output_cost_per_token: 0.0000016,    // $1.60 per 1M
            cache_creation_input_token_cost: 0.0000004,
            cache_read_input_token_cost: 0.0000001, // $0.10 per 1M
        });
        m.insert("gpt-4.1-nano-2025-04-14".to_string(), ModelPricing {
            input_cost_per_token: 0.0000001,     // $0.10 per 1M
            output_cost_per_token: 0.0000004,    // $0.40 per 1M
            cache_creation_input_token_cost: 0.0000001,
            cache_read_input_token_cost: 0.000000025, // $0.025 per 1M
        });
        m.insert("gpt-4.5-preview-2025-02-27".to_string(), ModelPricing {
            input_cost_per_token: 0.000075,      // $75.00 per 1M
            output_cost_per_token: 0.00015,      // $150.00 per 1M
            cache_creation_input_token_cost: 0.000075,
            cache_read_input_token_cost: 0.0000375, // $37.50 per 1M
        });
        
        // GPT-4o series
        m.insert("gpt-4o-2024-08-06".to_string(), ModelPricing {
            input_cost_per_token: 0.0000025,     // $2.50 per 1M
            output_cost_per_token: 0.00001,      // $10.00 per 1M
            cache_creation_input_token_cost: 0.0000025,
            cache_read_input_token_cost: 0.00000125, // $1.25 per 1M
        });
        m.insert("gpt-4o".to_string(), ModelPricing {
            input_cost_per_token: 0.0000025,
            output_cost_per_token: 0.00001,
            cache_creation_input_token_cost: 0.0000025,
            cache_read_input_token_cost: 0.00000125,
        });
        m.insert("gpt-4o-audio-preview-2024-12-17".to_string(), ModelPricing {
            input_cost_per_token: 0.0000025,     // $2.50 per 1M
            output_cost_per_token: 0.00001,      // $10.00 per 1M
            cache_creation_input_token_cost: 0.0000025,
            cache_read_input_token_cost: 0.0000025, // No cached rate, using input
        });
        m.insert("gpt-4o-realtime-preview-2025-06-03".to_string(), ModelPricing {
            input_cost_per_token: 0.000005,      // $5.00 per 1M
            output_cost_per_token: 0.00002,      // $20.00 per 1M
            cache_creation_input_token_cost: 0.000005,
            cache_read_input_token_cost: 0.0000025, // $2.50 per 1M
        });
        m.insert("gpt-4o-mini-2024-07-18".to_string(), ModelPricing {
            input_cost_per_token: 0.00000015,    // $0.15 per 1M
            output_cost_per_token: 0.0000006,    // $0.60 per 1M
            cache_creation_input_token_cost: 0.00000015,
            cache_read_input_token_cost: 0.000000075, // $0.075 per 1M
        });
        m.insert("gpt-4o-mini".to_string(), ModelPricing {
            input_cost_per_token: 0.00000015,
            output_cost_per_token: 0.0000006,
            cache_creation_input_token_cost: 0.00000015,
            cache_read_input_token_cost: 0.000000075,
        });
        m.insert("gpt-4o-mini-audio-preview-2024-12-17".to_string(), ModelPricing {
            input_cost_per_token: 0.00000015,    // $0.15 per 1M
            output_cost_per_token: 0.0000006,    // $0.60 per 1M
            cache_creation_input_token_cost: 0.00000015,
            cache_read_input_token_cost: 0.00000015, // No cached rate, using input
        });
        m.insert("gpt-4o-mini-realtime-preview-2024-12-17".to_string(), ModelPricing {
            input_cost_per_token: 0.0000006,     // $0.60 per 1M
            output_cost_per_token: 0.0000024,    // $2.40 per 1M
            cache_creation_input_token_cost: 0.0000006,
            cache_read_input_token_cost: 0.0000003, // $0.30 per 1M
        });
        m.insert("gpt-4o-search-preview-2025-03-11".to_string(), ModelPricing {
            input_cost_per_token: 0.0000025,     // $2.50 per 1M
            output_cost_per_token: 0.00001,      // $10.00 per 1M
            cache_creation_input_token_cost: 0.0000025,
            cache_read_input_token_cost: 0.0000025, // No cached rate, using input
        });
        m.insert("gpt-4o-mini-search-preview-2025-03-11".to_string(), ModelPricing {
            input_cost_per_token: 0.00000015,    // $0.15 per 1M
            output_cost_per_token: 0.0000006,    // $0.60 per 1M
            cache_creation_input_token_cost: 0.00000015,
            cache_read_input_token_cost: 0.00000015, // No cached rate, using input
        });
        
        // o1 series
        m.insert("o1-2024-12-17".to_string(), ModelPricing {
            input_cost_per_token: 0.000015,      // $15.00 per 1M
            output_cost_per_token: 0.00006,      // $60.00 per 1M
            cache_creation_input_token_cost: 0.000015,
            cache_read_input_token_cost: 0.0000075, // $7.50 per 1M
        });
        m.insert("o1".to_string(), ModelPricing {
            input_cost_per_token: 0.000015,
            output_cost_per_token: 0.00006,
            cache_creation_input_token_cost: 0.000015,
            cache_read_input_token_cost: 0.0000075,
        });
        m.insert("o1-pro-2025-03-19".to_string(), ModelPricing {
            input_cost_per_token: 0.00015,       // $150.00 per 1M
            output_cost_per_token: 0.0006,       // $600.00 per 1M
            cache_creation_input_token_cost: 0.00015,
            cache_read_input_token_cost: 0.00015, // No cached rate, using input
        });
        m.insert("o1-mini-2024-09-12".to_string(), ModelPricing {
            input_cost_per_token: 0.0000011,     // $1.10 per 1M
            output_cost_per_token: 0.0000044,    // $4.40 per 1M
            cache_creation_input_token_cost: 0.0000011,
            cache_read_input_token_cost: 0.00000055, // $0.55 per 1M
        });
        m.insert("o1-mini".to_string(), ModelPricing {
            input_cost_per_token: 0.0000011,
            output_cost_per_token: 0.0000044,
            cache_creation_input_token_cost: 0.0000011,
            cache_read_input_token_cost: 0.00000055,
        });
        
        // o3 series
        m.insert("o3-pro-2025-06-10".to_string(), ModelPricing {
            input_cost_per_token: 0.00002,       // $20.00 per 1M
            output_cost_per_token: 0.00008,      // $80.00 per 1M
            cache_creation_input_token_cost: 0.00002,
            cache_read_input_token_cost: 0.00002, // No cached rate, using input
        });
        m.insert("o3-2025-04-16".to_string(), ModelPricing {
            input_cost_per_token: 0.000002,      // $2.00 per 1M
            output_cost_per_token: 0.000008,     // $8.00 per 1M
            cache_creation_input_token_cost: 0.000002,
            cache_read_input_token_cost: 0.0000005, // $0.50 per 1M
        });
        m.insert("o3-deep-research-2025-06-26".to_string(), ModelPricing {
            input_cost_per_token: 0.00001,       // $10.00 per 1M
            output_cost_per_token: 0.00004,      // $40.00 per 1M
            cache_creation_input_token_cost: 0.00001,
            cache_read_input_token_cost: 0.0000025, // $2.50 per 1M
        });
        m.insert("o3-mini-2025-01-31".to_string(), ModelPricing {
            input_cost_per_token: 0.0000011,     // $1.10 per 1M
            output_cost_per_token: 0.0000044,    // $4.40 per 1M
            cache_creation_input_token_cost: 0.0000011,
            cache_read_input_token_cost: 0.00000055, // $0.55 per 1M
        });
        
        // o4 series
        m.insert("o4-mini-2025-04-16".to_string(), ModelPricing {
            input_cost_per_token: 0.0000011,     // $1.10 per 1M
            output_cost_per_token: 0.0000044,    // $4.40 per 1M
            cache_creation_input_token_cost: 0.0000011,
            cache_read_input_token_cost: 0.000000275, // $0.275 per 1M
        });
        m.insert("o4-mini-deep-research-2025-06-26".to_string(), ModelPricing {
            input_cost_per_token: 0.000002,      // $2.00 per 1M
            output_cost_per_token: 0.000008,     // $8.00 per 1M
            cache_creation_input_token_cost: 0.000002,
            cache_read_input_token_cost: 0.0000005, // $0.50 per 1M
        });
        
        // Codex models
        m.insert("codex-mini-latest".to_string(), ModelPricing {
            input_cost_per_token: 0.0000015,     // $1.50 per 1M
            output_cost_per_token: 0.000006,     // $6.00 per 1M
            cache_creation_input_token_cost: 0.0000015,
            cache_read_input_token_cost: 0.000000375, // $0.375 per 1M
        });
        
        // Special models
        m.insert("computer-use-preview-2025-03-11".to_string(), ModelPricing {
            input_cost_per_token: 0.000003,      // $3.00 per 1M
            output_cost_per_token: 0.000012,     // $12.00 per 1M
            cache_creation_input_token_cost: 0.000003,
            cache_read_input_token_cost: 0.000003, // No cached rate, using input
        });
        m.insert("gpt-image-1".to_string(), ModelPricing {
            input_cost_per_token: 0.000005,      // $5.00 per 1M
            output_cost_per_token: 0.0,          // No output cost
            cache_creation_input_token_cost: 0.000005,
            cache_read_input_token_cost: 0.00000125, // $1.25 per 1M
        });
        
        m
    };
}
