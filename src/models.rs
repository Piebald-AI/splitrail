use std::collections::HashMap;

use crate::types::ModelPricing;

// Hard-coded model info for now.
lazy_static::lazy_static! {
    pub static ref MODEL_PRICING: HashMap<String, ModelPricing> = {
        let mut m = HashMap::new();
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
        m
    };
}
