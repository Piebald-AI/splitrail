---
name: pricing
description: Guide for updating model pricing in Splitrail. Use when adding new AI model costs or updating existing pricing data.
---

# Pricing Model Updates

Token pricing is defined in `src/models.rs`. Built-in models are populated by `populate_defaults()` into the runtime model registry, and user/config models can be merged with `init_external_models()`.

## Adding a New Model

1. Add a `ModelInfo` entry in `populate_defaults()` with:
   - `pricing`: Use `PricingStructure::Flat { input_per_1m, output_per_1m }` for flat-rate models, or `PricingStructure::Tiered` for threshold-based pricing.
   - `caching`: Use the appropriate `CachingSupport` variant (`None`, `OpenAI`, `Anthropic`, `OpenAIWithWrites`, or `Tiered`).
   - `service_tiers`: Leave empty unless the provider has distinct priority/flex/batch rates.
   - `dated_pricing`: Leave empty unless pricing changes by usage date.
   - `is_estimated`: Set to `true` only when pricing is not confirmed by a provider/tool source.
   - `input_token_semantics`: Auto-derived for built-in models (defaults to `ExcludesCache`); only GPT/o-series models typically need `IncludesCacheRead`. External model configs should set this explicitly if it affects cache-read counting.
2. If the model has aliases (date suffixes, provider-prefixed names, regional names, etc.), add entries to the alias section mapping each observed name to the canonical model name.
3. Add or update tests in `src/models.rs` for canonical pricing, aliases, caching, and estimated/non-estimated status.

## Dated Pricing Overrides

Use `dated_pricing` when a model has temporary promotional pricing, launch pricing, or another time-bounded rate that should apply based on the original usage timestamp.

The default `pricing` and `caching` fields should represent the durable/current sticker price. Each `DatedPricing` override has a `valid_until: NaiveDate` exclusive end date and applies when `usage_date < valid_until`. For example, an introductory rate that applies through `2026-08-31` should use `valid_until = NaiveDate::from_ymd_opt(2026, 9, 1).expect("valid date")`.

When adding a dated override:

- Keep the model's default `pricing`/`caching` at the post-promo or durable rate.
- Add the temporary rate with `add_dated_pricing!()` after `add_model!()`.
- Add boundary tests for the final day of the temporary rate and the first day after it ends.
- Prefer dated calculator functions in analyzers that know the usage timestamp.

## Price Calculation

Use `models::calculate_total_cost_for_service_tier_at()` when an analyzer has a message or event timestamp. This preserves historical costs across dated pricing windows.

Use the undated compatibility helpers, such as `models::calculate_total_cost()`, only for tests, config-like calculations, or call sites that truly do not know the usage date. Undated helpers use the model's default `pricing`/`caching`, not temporary dated overrides.

## Common Pricing Sources

- [Anthropic pricing](https://www.anthropic.com/pricing)
- [OpenAI pricing](https://openai.com/pricing)
- [Google AI pricing](https://ai.google.dev/pricing)
