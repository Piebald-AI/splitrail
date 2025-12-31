# Pricing Model Updates

Token pricing is defined in `src/models.rs` using compile-time `phf` (perfect hash function) maps for fast lookups.

## Adding a New Model

1. Add a `ModelInfo` entry to `MODEL_INDEX` (line 65 in `src/models.rs`) with:
   - `pricing`: Use `PricingStructure::Flat { input_per_1m, output_per_1m }` for flat-rate models, or `PricingStructure::Tiered` for tiered pricing
   - `caching`: Use the appropriate `CachingSupport` variant (`None`, `OpenAI`, `Anthropic`, or `Google`)
   - `is_estimated`: Set to `true` if pricing is not officially published
2. If the model has aliases (date suffixes, etc.), add entries to `MODEL_ALIASES` mapping to the canonical model name

See existing entries in `src/models.rs` for the pattern.

## Price Calculation

Use `models::calculate_total_cost()` when an analyzer doesn't provide cost data.

## Common Pricing Sources

- Anthropic: https://www.anthropic.com/pricing
- OpenAI: https://openai.com/pricing
- Google: https://ai.google.dev/pricing
