# Pricing Model Updates

Token pricing is defined in `src/models.rs` using compile-time `phf` (perfect hash function) maps for fast lookups.

## Adding a New Model

1. Find the appropriate pricing map constant (e.g., `ANTHROPIC_PRICING`, `OPENAI_PRICING`) in `src/models.rs`
2. Add the model entry with pricing per million tokens: input, output, cache_creation, cache_read
3. If the model has aliases (date suffixes, etc.), add to `MODEL_ALIASES`
4. Add `ModelInfo` to `MODEL_INDEX` with pricing structure and caching support

See existing entries in `src/models.rs` for the pattern.

## Price Calculation

Use `models::calculate_total_cost()` when an analyzer doesn't provide cost data.

## Common Pricing Sources

- Anthropic: https://www.anthropic.com/pricing
- OpenAI: https://openai.com/pricing
- Google: https://ai.google.dev/pricing
