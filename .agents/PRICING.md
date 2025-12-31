# Pricing Model Updates

Token pricing is defined in `src/models.rs` using compile-time `phf` (perfect hash function) maps for fast lookups.

## Adding a New Model

1. Find the appropriate pricing constant (e.g., `ANTHROPIC_PRICING`, `OPENAI_PRICING`)

2. Add the model entry:

```rust
"model-name" => PricePerMillion {
    input: 3.00,           // USD per million input tokens
    output: 15.00,         // USD per million output tokens
    cache_creation: 3.75,  // USD per million cache creation tokens
    cache_read: 0.30,      // USD per million cache read tokens
},
```

3. If the model has aliases (date suffixes, etc.), add to `MODEL_ALIASES`:

```rust
"claude-sonnet-4-20250514" => "claude-sonnet-4",
```

## Model Info Structure

The `MODEL_INDEX` contains `ModelInfo` for each model:

```rust
ModelInfo {
    pricing: PricingStructure::Standard,  // or Batch, Reasoning, etc.
    supports_caching: true,
}
```

## Price Calculation

Use `models::calculate_total_cost()` when an analyzer doesn't provide cost data:

```rust
let cost = models::calculate_total_cost(
    &model_name,
    input_tokens,
    output_tokens,
    cache_creation_tokens,
    cache_read_tokens,
);
```

## Common Pricing Sources

- Anthropic: https://www.anthropic.com/pricing
- OpenAI: https://openai.com/pricing
- Google: https://ai.google.dev/pricing
