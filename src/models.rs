use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Timelike, Utc, Weekday};
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

/// Cache tier for models with separate, tiered write and read pricing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachingTierWithWrites {
    /// Maximum tokens for this caching tier (None means unlimited).
    pub max_tokens: Option<u64>,
    /// Cache write cost per 1M tokens.
    pub cache_write_per_1m: f64,
    /// Cache read cost per 1M tokens.
    pub cache_read_per_1m: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TieredCachingWithWrites {
    /// Cache tiers ordered from lowest threshold to highest.
    pub tiers: Vec<CachingTierWithWrites>,
    /// If true, bill the entire token count at the single matching tier's rate.
    pub bracket_pricing: bool,
}

/// Different cache pricing structures.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CachingSupport {
    /// Model does not support caching.
    None,
    /// Flat cached input pricing.
    OpenAI { cached_input_per_1m: f64 },
    /// Separate cache write and cache read pricing.
    Anthropic {
        cache_write_per_1m: f64,
        cache_read_per_1m: f64,
    },
    /// Separate cache write and cache read pricing for newer OpenAI models.
    OpenAIWithWrites {
        cache_write_per_1m: f64,
        cache_read_per_1m: f64,
    },
    /// Tiered cached input pricing.
    Tiered(TieredCaching),
    /// Tiered cache pricing with separate write and read rates.
    TieredWithWrites(TieredCachingWithWrites),
}

/// Provider service tier used for pricing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ServiceTier {
    /// Default on-demand pricing.
    #[default]
    Standard,
    /// Premium low-latency pricing.
    Priority,
    /// Discounted flexible-latency pricing.
    Flex,
    /// Discounted batch API pricing.
    Batch,
}

/// Pricing and caching for a specific provider service tier.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceTierPricing {
    pub pricing: PricingStructure,
    pub caching: CachingSupport,
}

/// Minutes in a day, the unit peak-window boundaries are expressed in.
const MINUTES_PER_DAY: u32 = 24 * 60;

/// Convert an `HH:MM` clock time into minutes after local midnight.
pub const fn clock(hour: u16, minute: u16) -> u16 {
    hour * 60 + minute
}

/// A recurring window during which a model is billed at its peak rates.
///
/// Boundaries are minutes after local midnight in the timezone of the owning
/// [`TimeOfDayPricing`]. `end_minute` is exclusive. An `end_minute` that is
/// less than or equal to `start_minute` describes a window that wraps past
/// midnight (for example 23:00 to 09:00); such a window is attributed to the
/// day on which it starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeakWindow {
    /// Minutes after local midnight at which the window starts (inclusive).
    pub start_minute: u16,
    /// Minutes after local midnight at which the window ends (exclusive).
    pub end_minute: u16,
    /// Weekdays the window applies to. Empty means every day of the week.
    #[serde(default)]
    pub weekdays: Vec<Weekday>,
}

impl PeakWindow {
    /// Build a window that applies Monday through Friday.
    pub fn weekdays(start_minute: u16, end_minute: u16) -> Self {
        Self {
            start_minute,
            end_minute,
            weekdays: vec![
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
            ],
        }
    }

    /// Whether `local` falls inside this window, honouring its weekday filter.
    fn matches<Tz: TimeZone>(&self, local: &DateTime<Tz>) -> bool {
        let minute = local.hour() * 60 + local.minute();
        let start = u32::from(self.start_minute);
        let end = u32::from(self.end_minute);

        let (in_window, day) = if start < end {
            (minute >= start && minute < end, local.weekday())
        } else if minute >= start {
            // Wrapping window, before midnight: it belongs to today.
            (true, local.weekday())
        } else if minute < end {
            // Wrapping window, after midnight: it belongs to yesterday.
            (true, local.weekday().pred())
        } else {
            (false, local.weekday())
        };

        in_window && (self.weekdays.is_empty() || self.weekdays.contains(&day))
    }
}

/// Peak and off-peak (time-of-day) pricing for a model.
///
/// The model's base `pricing` and `caching` describe peak rates. Usage that
/// falls outside every [`PeakWindow`] is billed at `off_peak_multiplier` times
/// those base rates, uniformly across input, output, and cache categories.
/// DeepSeek publishes off-peak rates at half of peak, so its multiplier is
/// `0.5`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimeOfDayPricing {
    /// IANA timezone the peak windows are expressed in, for example `UTC` or
    /// `Asia/Shanghai`. Unknown names fall back to UTC.
    #[serde(default = "default_pricing_timezone")]
    pub timezone: String,
    /// Windows billed at the model's base (peak) rates. An empty list means
    /// every hour is off-peak.
    #[serde(default)]
    pub peak_windows: Vec<PeakWindow>,
    /// Multiplier applied to base rates outside every peak window.
    #[serde(default = "default_off_peak_multiplier")]
    pub off_peak_multiplier: f64,
}

fn default_pricing_timezone() -> String {
    "UTC".to_string()
}

fn default_off_peak_multiplier() -> f64 {
    0.5
}

/// Pricing and caching that apply for usage before an exclusive end date.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatedPricing {
    pub valid_until: NaiveDate,
    pub pricing: PricingStructure,
    pub caching: CachingSupport,
    /// Optional service-tier rates for the same historical window.
    #[serde(default)]
    pub service_tiers: HashMap<ServiceTier, ServiceTierPricing>,
    /// Optional peak/off-peak schedule for the same historical window. Falls
    /// back to the model-level schedule when absent.
    #[serde(default)]
    pub time_of_day_pricing: Option<TimeOfDayPricing>,
}

/// How a provider reports input tokens relative to cache reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputTokenSemantics {
    /// Reported input tokens exclude cache reads and can be added to cached tokens.
    #[default]
    ExcludesCache,
    /// Reported input tokens include cache reads; subtract cache reads before normalizing.
    IncludesCacheRead,
}

/// Complete model information with all pricing details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Pricing structure (flat or tiered)
    pub pricing: PricingStructure,
    /// Caching support and pricing
    pub caching: CachingSupport,
    /// Optional overrides for provider service tiers such as Priority or Flex.
    #[serde(default)]
    pub service_tiers: HashMap<ServiceTier, ServiceTierPricing>,
    /// Optional dated standard-pricing overrides, ordered by exclusive end date.
    /// A dated override applies when the usage date is earlier than `valid_until`.
    #[serde(default)]
    pub dated_pricing: Vec<DatedPricing>,
    /// Optional peak/off-peak schedule. `pricing` and `caching` above are the
    /// peak rates; usage outside every peak window is discounted by
    /// [`TimeOfDayPricing::off_peak_multiplier`].
    #[serde(default)]
    pub time_of_day_pricing: Option<TimeOfDayPricing>,
    /// How provider usage reports input tokens relative to cache reads.
    #[serde(default)]
    pub input_token_semantics: InputTokenSemantics,
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
            if let Err(reason) = Self::validate_model_info(&info) {
                warn_once(format!(
                    "WARNING: init_external_models ignoring model `{name}`: {reason}"
                ));
                continue;
            }
            self.index.insert(name, Arc::new(info));
        }
        for (alias, canonical) in external_aliases {
            self.aliases.insert(alias, canonical);
        }
    }

    /// Validate a model definition, reporting the first problem found.
    ///
    /// Two periods ending on the same date are rejected outright. The resolver
    /// picks the period with the smallest `valid_until` among those still in
    /// force, so entries sharing a date would make the winner depend on vector
    /// order instead of on the data — a silent, order-dependent mispricing.
    fn validate_model_info(info: &ModelInfo) -> Result<(), String> {
        if !Self::validate_pricing_and_caching(&info.pricing, &info.caching) {
            return Err("invalid base pricing or caching tiers".to_string());
        }
        if !Self::validate_time_of_day(&info.time_of_day_pricing) {
            return Err("invalid time-of-day schedule".to_string());
        }
        if let Some((tier, _)) = info
            .service_tiers
            .iter()
            .find(|(_, tier)| !Self::validate_pricing_and_caching(&tier.pricing, &tier.caching))
        {
            return Err(format!(
                "invalid pricing or caching for service tier {tier:?}"
            ));
        }

        let mut seen: Vec<NaiveDate> = Vec::new();
        for dated in &info.dated_pricing {
            if seen.contains(&dated.valid_until) {
                return Err(format!(
                    "duplicate dated_pricing `valid_until` {}; each period must end on a \
                     distinct date",
                    dated.valid_until
                ));
            }
            seen.push(dated.valid_until);

            if !Self::validate_pricing_and_caching(&dated.pricing, &dated.caching) {
                return Err(format!(
                    "invalid pricing or caching for the period ending {}",
                    dated.valid_until
                ));
            }
            if !Self::validate_time_of_day(&dated.time_of_day_pricing) {
                return Err(format!(
                    "invalid time-of-day schedule for the period ending {}",
                    dated.valid_until
                ));
            }
            if let Some((tier, _)) = dated
                .service_tiers
                .iter()
                .find(|(_, tier)| !Self::validate_pricing_and_caching(&tier.pricing, &tier.caching))
            {
                return Err(format!(
                    "invalid pricing or caching for service tier {tier:?} in the period \
                     ending {}",
                    dated.valid_until
                ));
            }
        }

        Ok(())
    }

    /// A schedule is usable when its multiplier is a positive finite number and
    /// every window covers at least one minute of the day.
    fn validate_time_of_day(schedule: &Option<TimeOfDayPricing>) -> bool {
        let Some(schedule) = schedule else {
            return true;
        };

        schedule.off_peak_multiplier.is_finite()
            && schedule.off_peak_multiplier > 0.0
            && schedule.peak_windows.iter().all(|window| {
                u32::from(window.start_minute) < MINUTES_PER_DAY
                    && u32::from(window.end_minute) <= MINUTES_PER_DAY
                    && window.start_minute != window.end_minute
            })
    }

    fn validate_pricing_and_caching(pricing: &PricingStructure, caching: &CachingSupport) -> bool {
        let pricing_ok = match pricing {
            PricingStructure::Flat { .. } => true,
            PricingStructure::Tiered(tiered) => {
                Self::validate_tier_bounds(&tiered.tiers, |tier| tier.max_tokens)
            }
        };

        let caching_ok = match caching {
            CachingSupport::Tiered(tiered) => {
                Self::validate_tier_bounds(&tiered.tiers, |tier| tier.max_tokens)
            }
            CachingSupport::TieredWithWrites(tiered) => {
                !matches!(
                    pricing,
                    PricingStructure::Tiered(TieredPricing {
                        bracket_pricing: false,
                        ..
                    })
                ) && tiered.bracket_pricing
                    && Self::validate_tier_bounds(&tiered.tiers, |tier| tier.max_tokens)
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

fn input_token_semantics_for_model(model_name: &str) -> InputTokenSemantics {
    let name = model_name
        .rsplit_once('/')
        .map(|(_, name)| name)
        .unwrap_or(model_name);

    if name.starts_with("gpt-") || name.starts_with('o') {
        InputTokenSemantics::IncludesCacheRead
    } else {
        InputTokenSemantics::ExcludesCache
    }
}

/// Append a dated period, keeping `dated_pricing` sorted by `valid_until`.
///
/// Two periods may not end on the same date. The resolver picks the period with
/// the smallest `valid_until` among those still in force, so a duplicate would
/// make the winner depend on vector order rather than on the data. A silent
/// winner is a mispricing that no test would notice, so this is a hard error.
fn push_dated_pricing(model_info: &mut ModelInfo, name: &str, dated: DatedPricing) {
    assert!(
        !model_info
            .dated_pricing
            .iter()
            .any(|existing| existing.valid_until == dated.valid_until),
        "duplicate `add_dated_pricing!` for `{name}` ending {}: each period must end on a \
         distinct date",
        dated.valid_until
    );
    model_info.dated_pricing.push(dated);
    model_info
        .dated_pricing
        .sort_by_key(|dated| dated.valid_until);
}

/// Borrow the period ending at `valid_until`, or panic naming the caller.
///
/// Period-scoped overrides (a peak/off-peak schedule, a service-tier rate) must
/// be declared after the period they belong to. Without this check a typo in the
/// date would be a silent no-op, leaving the override quietly unapplied.
fn dated_period_mut<'a>(
    model_info: &'a mut ModelInfo,
    name: &str,
    valid_until: NaiveDate,
    macro_name: &str,
) -> &'a mut DatedPricing {
    model_info
        .dated_pricing
        .iter_mut()
        .find(|dated| dated.valid_until == valid_until)
        .unwrap_or_else(|| {
            panic!(
                "`{macro_name}!` for `{name}` found no period ending {valid_until}; call \
                 `add_dated_pricing!` with that date first"
            )
        })
}

/// Registers every built-in model and its published rates.
///
/// # Price provenance
///
/// Every rate below is transcribed from the vendor's own pricing page. The
/// `// Source:` comment immediately above a section header (for example
/// `// OpenAI Models`) is that vendor's canonical price list and covers **every**
/// entry in the section, including dated overrides (`add_dated_pricing!`) and
/// service-tier rates (`add_*_service_tier_pricing!`), which reuse the same
/// page.
///
/// An entry carries its own `// Source:` line only when its price comes from a
/// different page than the section default — typically a retired model that
/// keeps a per-model page after being dropped from the main table, or a model
/// served by a different provider (for example OpenAI weights on Amazon
/// Bedrock). Where the vendor no longer publishes a price at all, the marker
/// reads `unavailable (<reason>)` so the absence is explicit rather than
/// silently attributed to the wrong page.
///
/// # Periods and schedules
///
/// A model's rates change over time. `add_dated_pricing!` appends a period that
/// ends at an exclusive `valid_until`; the model's own `pricing` is the final
/// period and covers every date after the last one. Periods may be declared in
/// any order (they are kept sorted), but no two may end on the same date, and
/// the earliest period covers all of history before it.
///
/// A peak/off-peak schedule can be set for the model as a whole with
/// `add_time_of_day_pricing!(model, schedule)`, or for a single period with
/// `add_time_of_day_pricing!(model, valid_until, schedule)`. The model-level
/// schedule is the fallback for periods that carry none. Period-scoped
/// overrides — a schedule or a service-tier rate — must be declared after the
/// period they belong to; pointing at a date with no period is an error.
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
                    service_tiers: HashMap::new(),
                    dated_pricing: Vec::new(),
                    time_of_day_pricing: None,
                    input_token_semantics: input_token_semantics_for_model($name),
                    is_estimated: $est,
                }),
            );
        };
    }

    /// Add one dated price period, ending at the exclusive `valid_until`.
    ///
    /// Panics when the model already has a period ending on the same date; see
    /// [`push_dated_pricing`].
    macro_rules! add_dated_pricing {
        ($name:expr, $valid_until:expr, $pricing:expr, $caching:expr) => {
            if let Some(model_info) = index.get_mut($name)
                && let Some(model_info) = Arc::get_mut(model_info)
            {
                push_dated_pricing(
                    model_info,
                    $name,
                    DatedPricing {
                        valid_until: $valid_until,
                        pricing: $pricing,
                        caching: $caching,
                        service_tiers: HashMap::new(),
                        time_of_day_pricing: None,
                    },
                );
            }
        };
    }

    /// Attach a peak/off-peak schedule.
    ///
    /// Two forms:
    ///
    /// - `add_time_of_day_pricing!(model, schedule)` sets the model-level
    ///   schedule. It is the fallback, and applies to every period that carries
    ///   no schedule of its own.
    /// - `add_time_of_day_pricing!(model, valid_until, schedule)` sets the
    ///   schedule for one dated period, so several periods of the same model can
    ///   each run a different peak/off-peak rule. The period must already exist;
    ///   call `add_dated_pricing!` with the same date first.
    macro_rules! add_time_of_day_pricing {
        ($name:expr, $schedule:expr) => {
            if let Some(model_info) = index.get_mut($name)
                && let Some(model_info) = Arc::get_mut(model_info)
            {
                model_info.time_of_day_pricing = Some($schedule);
            }
        };
        ($name:expr, $valid_until:expr, $schedule:expr) => {
            if let Some(model_info) = index.get_mut($name)
                && let Some(model_info) = Arc::get_mut(model_info)
            {
                dated_period_mut(model_info, $name, $valid_until, "add_time_of_day_pricing")
                    .time_of_day_pricing = Some($schedule);
            }
        };
    }

    /// Attach a service-tier rate to one dated period. Like the period form of
    /// `add_time_of_day_pricing!`, the period must already exist; a missing
    /// period is a configuration error rather than a silent no-op.
    macro_rules! add_dated_service_tier_pricing {
        ($name:expr, $valid_until:expr, $service_tier:expr, $pricing:expr, $caching:expr) => {
            if let Some(model_info) = index.get_mut($name)
                && let Some(model_info) = Arc::get_mut(model_info)
            {
                dated_period_mut(
                    model_info,
                    $name,
                    $valid_until,
                    "add_dated_service_tier_pricing",
                )
                .service_tiers
                .insert(
                    $service_tier,
                    ServiceTierPricing {
                        pricing: $pricing,
                        caching: $caching,
                    },
                );
            }
        };
    }

    macro_rules! add_service_tier_pricing {
        ($name:expr, $service_tier:expr, $pricing:expr, $caching:expr) => {
            if let Some(model_info) = index.get_mut($name)
                && let Some(model_info) = Arc::get_mut(model_info)
            {
                model_info.service_tiers.insert(
                    $service_tier,
                    ServiceTierPricing {
                        pricing: $pricing,
                        caching: $caching,
                    },
                );
            }
        };
    }

    macro_rules! add_flat_service_tier_pricing {
        ($name:expr, $service_tier:expr, $input:expr, $cached:expr, $output:expr) => {
            add_service_tier_pricing!(
                $name,
                $service_tier,
                PricingStructure::Flat {
                    input_per_1m: $input,
                    output_per_1m: $output,
                },
                CachingSupport::OpenAI {
                    cached_input_per_1m: $cached,
                }
            );
        };
        ($name:expr, $service_tier:expr, $input:expr, $output:expr) => {
            add_service_tier_pricing!(
                $name,
                $service_tier,
                PricingStructure::Flat {
                    input_per_1m: $input,
                    output_per_1m: $output,
                },
                CachingSupport::None
            );
        };
    }

    macro_rules! add_tiered_service_tier_pricing_with_cache_writes {
        (
            $name:expr,
            $service_tier:expr,
            $short_input:expr,
            $short_cache_write:expr,
            $short_cache_read:expr,
            $short_output:expr,
            $long_input:expr,
            $long_cache_write:expr,
            $long_cache_read:expr,
            $long_output:expr
        ) => {
            add_service_tier_pricing!(
                $name,
                $service_tier,
                PricingStructure::Tiered(TieredPricing {
                    tiers: vec![
                        PricingTier {
                            max_tokens: Some(272_000),
                            input_per_1m: $short_input,
                            output_per_1m: $short_output,
                        },
                        PricingTier {
                            max_tokens: None,
                            input_per_1m: $long_input,
                            output_per_1m: $long_output,
                        },
                    ],
                    bracket_pricing: true,
                }),
                CachingSupport::TieredWithWrites(TieredCachingWithWrites {
                    tiers: vec![
                        CachingTierWithWrites {
                            max_tokens: Some(272_000),
                            cache_write_per_1m: $short_cache_write,
                            cache_read_per_1m: $short_cache_read,
                        },
                        CachingTierWithWrites {
                            max_tokens: None,
                            cache_write_per_1m: $long_cache_write,
                            cache_read_per_1m: $long_cache_read,
                        },
                    ],
                    bracket_pricing: true,
                })
            );
        };
    }

    macro_rules! add_tiered_service_tier_pricing {
        (
            $name:expr,
            $service_tier:expr,
            $short_input:expr,
            $short_cached:expr,
            $short_output:expr,
            $long_input:expr,
            $long_cached:expr,
            $long_output:expr
        ) => {
            add_service_tier_pricing!(
                $name,
                $service_tier,
                PricingStructure::Tiered(TieredPricing {
                    tiers: vec![
                        PricingTier {
                            max_tokens: Some(272_000),
                            input_per_1m: $short_input,
                            output_per_1m: $short_output,
                        },
                        PricingTier {
                            max_tokens: None,
                            input_per_1m: $long_input,
                            output_per_1m: $long_output,
                        },
                    ],
                    bracket_pricing: true,
                }),
                CachingSupport::Tiered(TieredCaching {
                    tiers: vec![
                        CachingTier {
                            max_tokens: Some(272_000),
                            cached_input_per_1m: $short_cached,
                        },
                        CachingTier {
                            max_tokens: None,
                            cached_input_per_1m: $long_cached,
                        },
                    ],
                    bracket_pricing: true,
                })
            );
        };
        (
            $name:expr,
            $service_tier:expr,
            $short_input:expr,
            $short_output:expr,
            $long_input:expr,
            $long_output:expr
        ) => {
            add_service_tier_pricing!(
                $name,
                $service_tier,
                PricingStructure::Tiered(TieredPricing {
                    tiers: vec![
                        PricingTier {
                            max_tokens: Some(272_000),
                            input_per_1m: $short_input,
                            output_per_1m: $short_output,
                        },
                        PricingTier {
                            max_tokens: None,
                            input_per_1m: $long_input,
                            output_per_1m: $long_output,
                        },
                    ],
                    bracket_pricing: true,
                }),
                CachingSupport::None
            );
        };
    }

    // OpenAI Models
    // Source: https://developers.openai.com/api/docs/pricing
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
    // Source: https://developers.openai.com/api/docs/models/o1-preview
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
    // Source: https://developers.openai.com/api/docs/models/o1-mini
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
            output_per_1m: 15.0
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
    // Source: https://developers.openai.com/api/docs/models/codex-mini-latest
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
    // Source: unavailable (OpenAI no longer publishes this model)
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
    // Source: unavailable (OpenAI no longer publishes this model)
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
    // Source: https://developers.openai.com/api/docs/models/gpt-5.1-codex
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
    // Source: https://developers.openai.com/api/docs/models/gpt-5.1-codex-mini
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
    // Source: https://developers.openai.com/api/docs/models/gpt-5.1-codex-max
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
    // Source: https://developers.openai.com/api/docs/models/gpt-5.2-codex
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
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
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
            bracket_pricing: true,
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
            bracket_pricing: true,
        }),
        CachingSupport::None,
        false
    );

    add_model!(
        "gpt-5.5",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 5.0,
                    output_per_1m: 30.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 10.0,
                    output_per_1m: 45.0
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(272_000),
                    cached_input_per_1m: 0.50
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 1.00
                },
            ],
            bracket_pricing: true,
        }),
        false
    );

    add_model!(
        "gpt-5.5-pro",
        PricingStructure::Flat {
            input_per_1m: 30.0,
            output_per_1m: 180.0
        },
        CachingSupport::None,
        false
    );

    // GPT-6 Astra uses OpenAI's whole-request long-context bracket: once the
    // prompt (uncached input plus cache reads) exceeds 272K tokens, every input,
    // output, cache-write, and cache-read token in the request is billed at the
    // long-context rate. Keeping matching tier boundaries across pricing and
    // caching lets the shared calculator make that decision once for the whole
    // request rather than accidentally pricing each token category separately.
    add_model!(
        "gpt-6-astra",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 10.0,
                    output_per_1m: 50.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 20.0,
                    output_per_1m: 75.0
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 12.50,
                    cache_read_per_1m: 1.0
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 25.0,
                    cache_read_per_1m: 2.0
                },
            ],
            bracket_pricing: true,
        }),
        false
    );

    add_model!(
        "gpt-5.6-sol",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 4.0,
                    output_per_1m: 20.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 8.0,
                    output_per_1m: 30.0
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 5.0,
                    cache_read_per_1m: 0.40
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 10.0,
                    cache_read_per_1m: 0.80
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_dated_pricing!(
        "gpt-5.6-sol",
        NaiveDate::from_ymd_opt(2026, 8, 21).expect("valid date"),
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 5.0,
                    output_per_1m: 30.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 10.0,
                    output_per_1m: 45.0
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 6.25,
                    cache_read_per_1m: 0.50
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 12.50,
                    cache_read_per_1m: 1.0
                },
            ],
            bracket_pricing: true,
        })
    );
    add_model!(
        "gpt-5.6-terra",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
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
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 2.5,
                    cache_read_per_1m: 0.20
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 5.0,
                    cache_read_per_1m: 0.40
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_dated_pricing!(
        "gpt-5.6-terra",
        NaiveDate::from_ymd_opt(2026, 7, 30).expect("valid date"),
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
                    output_per_1m: 22.50
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 3.125,
                    cache_read_per_1m: 0.25
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 6.25,
                    cache_read_per_1m: 0.50
                },
            ],
            bracket_pricing: true,
        })
    );
    add_model!(
        "gpt-5.6-luna",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 0.20,
                    output_per_1m: 1.20
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 0.40,
                    output_per_1m: 1.80
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 0.25,
                    cache_read_per_1m: 0.02
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 0.50,
                    cache_read_per_1m: 0.04
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_dated_pricing!(
        "gpt-5.6-luna",
        NaiveDate::from_ymd_opt(2026, 7, 30).expect("valid date"),
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 1.0,
                    output_per_1m: 6.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.0,
                    output_per_1m: 9.0
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 1.25,
                    cache_read_per_1m: 0.10
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 2.50,
                    cache_read_per_1m: 0.20
                },
            ],
            bracket_pricing: true,
        })
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

    // OpenAI open-weight safety models on Amazon Bedrock
    // Source: https://aws.amazon.com/bedrock/pricing/
    add_model!(
        "gpt-oss-safeguard-120b",
        PricingStructure::Flat {
            input_per_1m: 0.15,
            output_per_1m: 0.60
        },
        CachingSupport::None,
        false
    );

    // OpenAI service tiers and dated overrides. All rates below come from the
    // OpenAI pricing page cited at the top of this section.
    //
    // OpenAI publishes Fast mode at 2x Standard and Flex/Batch at 0.5x.
    // Splitrail's existing `Priority` tier represents that premium low-latency
    // class, so map Fast pricing there while preserving the provider-neutral
    // service-tier vocabulary used by analyzers and callers.
    add_tiered_service_tier_pricing_with_cache_writes!(
        "gpt-6-astra",
        ServiceTier::Priority,
        20.0,
        25.0,
        2.0,
        100.0,
        40.0,
        50.0,
        4.0,
        150.0
    );
    for service_tier in [ServiceTier::Flex, ServiceTier::Batch] {
        add_tiered_service_tier_pricing_with_cache_writes!(
            "gpt-6-astra",
            service_tier,
            5.0,
            6.25,
            0.50,
            25.0,
            10.0,
            12.50,
            1.0,
            37.50
        );
    }

    add_tiered_service_tier_pricing_with_cache_writes!(
        "gpt-5.6-sol",
        ServiceTier::Priority,
        8.0,
        10.0,
        0.80,
        40.0,
        16.0,
        20.0,
        1.60,
        60.0
    );
    add_tiered_service_tier_pricing_with_cache_writes!(
        "gpt-5.6-terra",
        ServiceTier::Priority,
        4.0,
        5.0,
        0.40,
        24.0,
        8.0,
        10.0,
        0.80,
        36.0
    );
    add_tiered_service_tier_pricing_with_cache_writes!(
        "gpt-5.6-luna",
        ServiceTier::Priority,
        0.40,
        0.50,
        0.04,
        2.40,
        0.80,
        1.0,
        0.08,
        3.60
    );
    let sol_promotion = NaiveDate::from_ymd_opt(2026, 8, 21).expect("valid date");
    add_dated_service_tier_pricing!(
        "gpt-5.6-sol",
        sol_promotion,
        ServiceTier::Priority,
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 10.0,
                    output_per_1m: 60.0,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 20.0,
                    output_per_1m: 90.0,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 12.50,
                    cache_read_per_1m: 1.0,
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 25.0,
                    cache_read_per_1m: 2.0,
                },
            ],
            bracket_pricing: true,
        })
    );
    for service_tier in [ServiceTier::Flex, ServiceTier::Batch] {
        add_dated_service_tier_pricing!(
            "gpt-5.6-sol",
            sol_promotion,
            service_tier,
            PricingStructure::Tiered(TieredPricing {
                tiers: vec![
                    PricingTier {
                        max_tokens: Some(272_000),
                        input_per_1m: 2.50,
                        output_per_1m: 15.0,
                    },
                    PricingTier {
                        max_tokens: None,
                        input_per_1m: 5.0,
                        output_per_1m: 22.50,
                    },
                ],
                bracket_pricing: true,
            }),
            CachingSupport::TieredWithWrites(TieredCachingWithWrites {
                tiers: vec![
                    CachingTierWithWrites {
                        max_tokens: Some(272_000),
                        cache_write_per_1m: 3.125,
                        cache_read_per_1m: 0.25,
                    },
                    CachingTierWithWrites {
                        max_tokens: None,
                        cache_write_per_1m: 6.25,
                        cache_read_per_1m: 0.50,
                    },
                ],
                bracket_pricing: true,
            })
        );
    }

    let pre_cut = NaiveDate::from_ymd_opt(2026, 7, 30).expect("valid date");
    add_dated_service_tier_pricing!(
        "gpt-5.6-terra",
        pre_cut,
        ServiceTier::Priority,
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 5.0,
                    output_per_1m: 30.0,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 10.0,
                    output_per_1m: 45.0,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 6.25,
                    cache_read_per_1m: 0.50,
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 12.50,
                    cache_read_per_1m: 1.0,
                },
            ],
            bracket_pricing: true,
        })
    );
    add_dated_service_tier_pricing!(
        "gpt-5.6-luna",
        pre_cut,
        ServiceTier::Priority,
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(272_000),
                    input_per_1m: 2.0,
                    output_per_1m: 12.0,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 4.0,
                    output_per_1m: 18.0,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::TieredWithWrites(TieredCachingWithWrites {
            tiers: vec![
                CachingTierWithWrites {
                    max_tokens: Some(272_000),
                    cache_write_per_1m: 2.50,
                    cache_read_per_1m: 0.20,
                },
                CachingTierWithWrites {
                    max_tokens: None,
                    cache_write_per_1m: 5.0,
                    cache_read_per_1m: 0.40,
                },
            ],
            bracket_pricing: true,
        })
    );
    for service_tier in [ServiceTier::Flex, ServiceTier::Batch] {
        add_dated_service_tier_pricing!(
            "gpt-5.6-terra",
            pre_cut,
            service_tier,
            PricingStructure::Tiered(TieredPricing {
                tiers: vec![
                    PricingTier {
                        max_tokens: Some(272_000),
                        input_per_1m: 1.25,
                        output_per_1m: 7.50,
                    },
                    PricingTier {
                        max_tokens: None,
                        input_per_1m: 2.50,
                        output_per_1m: 11.25,
                    },
                ],
                bracket_pricing: true,
            }),
            CachingSupport::TieredWithWrites(TieredCachingWithWrites {
                tiers: vec![
                    CachingTierWithWrites {
                        max_tokens: Some(272_000),
                        cache_write_per_1m: 1.5625,
                        cache_read_per_1m: 0.125,
                    },
                    CachingTierWithWrites {
                        max_tokens: None,
                        cache_write_per_1m: 3.125,
                        cache_read_per_1m: 0.25,
                    },
                ],
                bracket_pricing: true,
            })
        );
        add_dated_service_tier_pricing!(
            "gpt-5.6-luna",
            pre_cut,
            service_tier,
            PricingStructure::Tiered(TieredPricing {
                tiers: vec![
                    PricingTier {
                        max_tokens: Some(272_000),
                        input_per_1m: 0.50,
                        output_per_1m: 3.0,
                    },
                    PricingTier {
                        max_tokens: None,
                        input_per_1m: 1.0,
                        output_per_1m: 4.50,
                    },
                ],
                bracket_pricing: true,
            }),
            CachingSupport::TieredWithWrites(TieredCachingWithWrites {
                tiers: vec![
                    CachingTierWithWrites {
                        max_tokens: Some(272_000),
                        cache_write_per_1m: 0.625,
                        cache_read_per_1m: 0.05,
                    },
                    CachingTierWithWrites {
                        max_tokens: None,
                        cache_write_per_1m: 1.25,
                        cache_read_per_1m: 0.10,
                    },
                ],
                bracket_pricing: true,
            })
        );
    }

    add_flat_service_tier_pricing!("gpt-5.5", ServiceTier::Priority, 12.50, 1.25, 75.0);
    add_flat_service_tier_pricing!("gpt-5.4", ServiceTier::Priority, 5.0, 0.50, 30.0);
    add_flat_service_tier_pricing!("gpt-5.4-mini", ServiceTier::Priority, 1.50, 0.15, 9.0);

    for service_tier in [ServiceTier::Flex, ServiceTier::Batch] {
        add_tiered_service_tier_pricing_with_cache_writes!(
            "gpt-5.6-sol",
            service_tier,
            2.0,
            2.50,
            0.20,
            10.0,
            4.0,
            5.0,
            0.40,
            15.0
        );
        add_tiered_service_tier_pricing_with_cache_writes!(
            "gpt-5.6-terra",
            service_tier,
            1.0,
            1.25,
            0.10,
            6.0,
            2.0,
            2.50,
            0.20,
            9.0
        );
        add_tiered_service_tier_pricing_with_cache_writes!(
            "gpt-5.6-luna",
            service_tier,
            0.10,
            0.125,
            0.01,
            0.60,
            0.20,
            0.25,
            0.02,
            0.90
        );
        add_tiered_service_tier_pricing!(
            "gpt-5.5",
            service_tier,
            2.50,
            0.25,
            15.0,
            5.0,
            0.50,
            22.50
        );
        add_flat_service_tier_pricing!("gpt-5.5-pro", service_tier, 15.0, 90.0);
        add_tiered_service_tier_pricing!(
            "gpt-5.4",
            service_tier,
            1.25,
            0.13,
            7.50,
            2.50,
            0.25,
            11.25
        );
        add_flat_service_tier_pricing!("gpt-5.4-mini", service_tier, 0.375, 0.0375, 2.25);
        add_flat_service_tier_pricing!("gpt-5.4-nano", service_tier, 0.10, 0.01, 0.625);
        add_tiered_service_tier_pricing!("gpt-5.4-pro", service_tier, 15.0, 90.0, 30.0, 135.0);
    }

    // Anthropic Models
    // Source: https://docs.claude.com/en/docs/about-claude/pricing
    add_model!(
        "claude-fable-5-1",
        PricingStructure::Flat {
            input_per_1m: 10.0,
            output_per_1m: 50.0
        },
        // Splitrail receives one undifferentiated cache-creation count, so use
        // Anthropic's default 5-minute write rate here. The optional one-hour
        // cache write remains $20/MTok, but cannot be selected without losing
        // the token source's TTL information before pricing.
        CachingSupport::Anthropic {
            cache_write_per_1m: 12.5,
            cache_read_per_1m: 0.25
        },
        false
    );
    // Anthropic's Batch discount stacks with prompt-caching prices. Apply its
    // 50% discount to the same default five-minute write approximation used by
    // standard pricing so Batch usage with cache tokens is not undercounted.
    add_service_tier_pricing!(
        "claude-fable-5-1",
        ServiceTier::Batch,
        PricingStructure::Flat {
            input_per_1m: 5.0,
            output_per_1m: 25.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 6.25,
            cache_read_per_1m: 0.125
        }
    );
    add_model!(
        "claude-fable-5",
        PricingStructure::Flat {
            input_per_1m: 10.0,
            output_per_1m: 50.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 12.5,
            cache_read_per_1m: 1.0
        },
        false
    );
    add_model!(
        "claude-sonnet-5",
        PricingStructure::Flat {
            input_per_1m: 2.0,
            output_per_1m: 10.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 2.5,
            cache_read_per_1m: 0.2
        },
        false
    );
    add_model!(
        "claude-opus-5",
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
        "claude-opus-4-8",
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
    // Source: unavailable (retired from the Anthropic pricing page)
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
    // Source: unavailable (retired from the Anthropic pricing page)
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
    // Source: unavailable (retired from the Anthropic pricing page)
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
    // Source: unavailable (retired from the Anthropic pricing page)
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
    // Source: https://ai.google.dev/gemini-api/docs/pricing
    add_model!(
        "gemini-3-flash-preview",
        PricingStructure::Flat {
            input_per_1m: 0.5,
            output_per_1m: 3.0
        },
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.05
            }],
            bracket_pricing: false,
        }),
        false
    );
    // Gemini 3.8 Flash ships on promotional rates that revert to the standard
    // rates on 2027-01-01, so the base entry carries the standard rates and the
    // dated override covers the promotional window.
    add_model!(
        "gemini-3.8-flash",
        PricingStructure::Flat {
            input_per_1m: 1.50,
            output_per_1m: 7.50
        },
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.15
            }],
            bracket_pricing: false,
        }),
        false
    );
    add_dated_pricing!(
        "gemini-3.8-flash",
        NaiveDate::from_ymd_opt(2027, 1, 1).expect("valid date"),
        PricingStructure::Flat {
            input_per_1m: 0.75,
            output_per_1m: 3.75
        },
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.075
            }],
            bracket_pricing: false,
        })
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
        CachingSupport::Tiered(TieredCaching {
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
    // Source: unavailable (retired from the Gemini API pricing page)
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
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.125
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.25
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
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.03
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
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.01
            }],
            bracket_pricing: false,
        }),
        false
    );
    // Source: unavailable (retired from the Gemini API pricing page)
    add_model!(
        "gemini-2.0-pro-exp-02-05",
        PricingStructure::Flat {
            input_per_1m: 0.0,
            output_per_1m: 0.0
        },
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.0
            }],
            bracket_pricing: false,
        }),
        false
    );
    // Source: unavailable (retired from the Gemini API pricing page)
    add_model!(
        "gemini-2.0-flash",
        PricingStructure::Flat {
            input_per_1m: 0.1,
            output_per_1m: 0.4
        },
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![CachingTier {
                max_tokens: None,
                cached_input_per_1m: 0.025
            }],
            bracket_pricing: false,
        }),
        false
    );
    // Source: unavailable (retired from the Gemini API pricing page)
    add_model!(
        "gemini-2.0-flash-lite",
        PricingStructure::Flat {
            input_per_1m: 0.075,
            output_per_1m: 0.3
        },
        CachingSupport::None,
        false
    );
    // Source: unavailable (retired from the Gemini API pricing page)
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
        CachingSupport::Tiered(TieredCaching {
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
    // Source: unavailable (retired from the Gemini API pricing page)
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
        CachingSupport::Tiered(TieredCaching {
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
    // Source: unavailable (retired from the Gemini API pricing page)
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
        CachingSupport::Tiered(TieredCaching {
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
    // Source: https://docs.z.ai/guides/overview/pricing
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
    // Source: https://docs.x.ai/developers/pricing
    add_model!(
        "grok-4.3",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 1.25,
                    output_per_1m: 2.50,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.50,
                    output_per_1m: 5.00,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.20,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.40,
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_model!(
        "grok-4.20-0309-reasoning",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 1.25,
                    output_per_1m: 2.50,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.50,
                    output_per_1m: 5.00,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.20,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.40,
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_model!(
        "grok-4.20-0309-non-reasoning",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 1.25,
                    output_per_1m: 2.50,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.50,
                    output_per_1m: 5.00,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.20,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.40,
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_model!(
        "grok-4.20-multi-agent-0309",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 1.25,
                    output_per_1m: 2.50,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.50,
                    output_per_1m: 5.00,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.20,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.40,
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_model!(
        "grok-4.5",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 2.00,
                    output_per_1m: 6.00,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 4.00,
                    output_per_1m: 12.00,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.30,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.60,
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_model!(
        "grok-4.6",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 2.00,
                    output_per_1m: 6.00,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 4.00,
                    output_per_1m: 12.00,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.50,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 1.00,
                },
            ],
            bracket_pricing: true,
        }),
        false
    );
    add_model!(
        "grok-build-0.1",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(200_000),
                    input_per_1m: 1.00,
                    output_per_1m: 2.00,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.00,
                    output_per_1m: 4.00,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(200_000),
                    cached_input_per_1m: 0.20,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.40,
                },
            ],
            bracket_pricing: true,
        }),
        false
    );

    // Synthetic.new Models
    // Synthetic.new bills a flat subscription ($1/day or $30/month) with no
    // per-token billing, so it publishes no per-token price to cite. The
    // figures below are carried over from the upstream model vendors.
    // Source: unavailable (subscription-only vendor; no per-token price published)
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
    // Volcano Ark publishes CNY rates bracketed on input length
    // (3.20 / 0.64 / 16.00, then 4.80 / 0.96 / 24.00, then 9.60 / 1.92 / 48.00
    // per 1M tokens) and bills every token in a request at its bracket's rate.
    // The USD figures below apply the same 7 CNY-per-USD conversion as the
    // other CNY-only providers in this file, so they remain an estimate.
    // Source: https://www.volcengine.com/docs/82379/1544106
    add_model!(
        "doubao-seed-2.0-code",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(32_000),
                    input_per_1m: 0.457,
                    output_per_1m: 2.286,
                },
                PricingTier {
                    max_tokens: Some(128_000),
                    input_per_1m: 0.686,
                    output_per_1m: 3.429,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 1.371,
                    output_per_1m: 6.857,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(32_000),
                    cached_input_per_1m: 0.091,
                },
                CachingTier {
                    max_tokens: Some(128_000),
                    cached_input_per_1m: 0.137,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.274,
                },
            ],
            bracket_pricing: true,
        }),
        true
    );

    // DeepSeek Models
    // Source: https://api-docs.deepseek.com/quick_start/pricing/
    // The rates below are the published peak rates; off-peak rates are half of
    // peak and apply outside the peak windows (see below).
    add_model!(
        "deepseek-v4-pro",
        PricingStructure::Flat {
            input_per_1m: 1.32,
            output_per_1m: 3.96
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.044
        },
        false
    );
    // DeepSeek now asks callers to use `deepseek-flash`; the legacy
    // `deepseek-v4-flash` name is still accepted and billed at the same rate.
    // Requests to `deepseek-v4-pro` keep being served by DeepSeek-V4-Pro.
    add_model!(
        "deepseek-v4-flash",
        PricingStructure::Flat {
            input_per_1m: 0.30,
            output_per_1m: 1.20
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.006
        },
        false
    );
    // Peak hours are 01:00-04:00 and 06:00-10:00 UTC, Monday through Friday;
    // every other hour, including all weekend hours, is billed at half rate.
    for model in ["deepseek-v4-pro", "deepseek-v4-flash"] {
        add_time_of_day_pricing!(
            model,
            TimeOfDayPricing {
                timezone: "UTC".to_string(),
                peak_windows: vec![
                    PeakWindow::weekdays(clock(1, 0), clock(4, 0)),
                    PeakWindow::weekdays(clock(6, 0), clock(10, 0)),
                ],
                off_peak_multiplier: 0.5,
            }
        );
    }
    // Amazon Bedrock model ID. DeepSeek's own pricing page does not cover it.
    // Source: unavailable (third-party model id; DeepSeek publishes no price for it)
    add_model!(
        "deepseek.v3.2",
        PricingStructure::Flat {
            input_per_1m: 0.62,
            output_per_1m: 1.85
        },
        CachingSupport::None,
        false
    );

    // Z.AI (Zhipu AI) - Additional Models
    // Source: https://docs.z.ai/guides/overview/pricing
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
        "glm-5.1",
        PricingStructure::Flat {
            input_per_1m: 1.40,
            output_per_1m: 4.40
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.26
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
    // Z.AI's current flagship line and the remaining text/vision tiers.
    add_model!(
        "glm-5.3",
        PricingStructure::Flat {
            input_per_1m: 1.4,
            output_per_1m: 4.4
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.26
        },
        false
    );
    add_model!(
        "glm-5.3-flash",
        PricingStructure::Flat {
            input_per_1m: 0.15,
            output_per_1m: 0.50
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.03
        },
        false
    );
    add_model!(
        "glm-5.2",
        PricingStructure::Flat {
            input_per_1m: 1.4,
            output_per_1m: 4.4
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.26
        },
        false
    );
    add_model!(
        "glm-4.7-flashx",
        PricingStructure::Flat {
            input_per_1m: 0.07,
            output_per_1m: 0.40
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.01
        },
        false
    );
    add_model!(
        "glm-4.5",
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
        "glm-4.5-x",
        PricingStructure::Flat {
            input_per_1m: 2.20,
            output_per_1m: 8.90
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.45
        },
        false
    );
    add_model!(
        "glm-4.5-airx",
        PricingStructure::Flat {
            input_per_1m: 1.10,
            output_per_1m: 4.50
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.22
        },
        false
    );
    add_model!(
        "glm-4.5v",
        PricingStructure::Flat {
            input_per_1m: 0.60,
            output_per_1m: 1.80
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.11
        },
        false
    );
    add_model!(
        "glm-4.6v-flashx",
        PricingStructure::Flat {
            input_per_1m: 0.04,
            output_per_1m: 0.40
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.004
        },
        false
    );
    // GLM-OCR is priced per token on both directions and has no cache tier.
    add_model!(
        "glm-ocr",
        PricingStructure::Flat {
            input_per_1m: 0.03,
            output_per_1m: 0.03
        },
        CachingSupport::None,
        false
    );

    // Xiaomi Models
    // Source: https://mimo.mi.com/docs/zh-CN/pricing
    add_model!(
        "mimo-v2.5-pro",
        PricingStructure::Flat {
            input_per_1m: 0.435,
            output_per_1m: 0.87
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.0036
        },
        true
    );
    // Source: unavailable (Xiaomi publishes only mimo-v2.5-pro and mimo-v2.5)
    add_model!(
        "mimo-v2-omni",
        PricingStructure::Flat {
            input_per_1m: 0.40,
            output_per_1m: 2.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.08
        },
        true
    );

    // MiniMax Models
    // Source: https://platform.minimax.io/docs/guides/pricing-paygo
    add_model!(
        "minimax-m2.1",
        PricingStructure::Flat {
            input_per_1m: 0.30,
            output_per_1m: 1.20
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 0.375,
            cache_read_per_1m: 0.03
        },
        false
    );
    add_model!(
        "minimax-m2.7",
        PricingStructure::Flat {
            input_per_1m: 0.30,
            output_per_1m: 1.20
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 0.375,
            cache_read_per_1m: 0.06
        },
        false
    );
    add_model!(
        "minimax-m2.5",
        PricingStructure::Flat {
            input_per_1m: 0.30,
            output_per_1m: 1.20
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 0.375,
            cache_read_per_1m: 0.03
        },
        false
    );
    // MiniMax-M3 is bracketed on input size: at or below 512k tokens the
    // permanent 50% discount applies, above it the undiscounted rates apply.
    add_model!(
        "minimax-m3",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(512_000),
                    input_per_1m: 0.30,
                    output_per_1m: 1.20,
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 0.60,
                    output_per_1m: 2.40,
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Tiered(TieredCaching {
            tiers: vec![
                CachingTier {
                    max_tokens: Some(512_000),
                    cached_input_per_1m: 0.06,
                },
                CachingTier {
                    max_tokens: None,
                    cached_input_per_1m: 0.12,
                },
            ],
            bracket_pricing: true,
        }),
        false
    );

    // Moonshot AI Models
    // Source: https://platform.kimi.ai/docs/pricing/chat
    add_model!(
        "kimi-k3",
        PricingStructure::Flat {
            input_per_1m: 3.0,
            output_per_1m: 15.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.30
        },
        false
    );
    add_model!(
        "kimi-k2.7-code",
        PricingStructure::Flat {
            input_per_1m: 0.95,
            output_per_1m: 4.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.19
        },
        false
    );
    add_model!(
        "kimi-k2.7-code-highspeed",
        PricingStructure::Flat {
            input_per_1m: 1.90,
            output_per_1m: 8.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.38
        },
        false
    );
    add_model!(
        "kimi-k2.6",
        PricingStructure::Flat {
            input_per_1m: 0.95,
            output_per_1m: 4.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.16
        },
        false
    );
    // Source: unavailable (retired 2026-08-31; Moonshot no longer publishes a price)
    add_model!(
        "kimi-k2.5",
        PricingStructure::Flat {
            input_per_1m: 0.60,
            output_per_1m: 3.0
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.10
        },
        false
    );

    // Qwen Models
    // Source: https://www.alibabacloud.com/help/en/model-studio/model-pricing
    add_model!(
        "qwen3.6-plus",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(256_000),
                    input_per_1m: 0.50,
                    output_per_1m: 3.0
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 2.0,
                    output_per_1m: 6.0
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Anthropic {
            cache_write_per_1m: 0.625,
            cache_read_per_1m: 0.05
        },
        false
    );
    // Source: unavailable (absent from the Model Studio pricing table)
    add_model!(
        "qwen3.5-35b-a3b",
        PricingStructure::Flat {
            input_per_1m: 0.1625,
            output_per_1m: 1.30
        },
        CachingSupport::None,
        true
    );
    add_model!(
        "qwen3.7-plus",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(256_000),
                    input_per_1m: 0.32,
                    output_per_1m: 1.28
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 0.96,
                    output_per_1m: 3.84
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Anthropic {
            cache_write_per_1m: 0.40,
            cache_read_per_1m: 0.064
        },
        true
    );
    // Source: unavailable (absent from the Model Studio pricing table)
    add_model!(
        "qwen3.7-flash",
        PricingStructure::Tiered(TieredPricing {
            tiers: vec![
                PricingTier {
                    max_tokens: Some(32_000),
                    input_per_1m: 0.03,
                    output_per_1m: 0.13
                },
                PricingTier {
                    max_tokens: Some(256_000),
                    input_per_1m: 0.10,
                    output_per_1m: 0.40
                },
                PricingTier {
                    max_tokens: None,
                    input_per_1m: 0.20,
                    output_per_1m: 0.80
                },
            ],
            bracket_pricing: true,
        }),
        CachingSupport::Anthropic {
            cache_write_per_1m: 0.038,
            cache_read_per_1m: 0.006
        },
        true
    );

    // International-scope list prices. Explicit cache creation bills at 125% of
    // the input rate and cache hits at 10%, matching the Qwen3.6/3.7 entries.
    add_model!(
        "qwen3.8-max",
        PricingStructure::Flat {
            input_per_1m: 2.0,
            output_per_1m: 6.0
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 2.5,
            cache_read_per_1m: 0.20
        },
        false
    );
    add_model!(
        "qwen3.7-max",
        PricingStructure::Flat {
            input_per_1m: 2.5,
            output_per_1m: 7.5
        },
        CachingSupport::Anthropic {
            cache_write_per_1m: 3.125,
            cache_read_per_1m: 0.25
        },
        false
    );

    // Meituan Models
    // Source: unavailable (Meituan publishes no public per-token price)
    add_model!(
        "longcat-flash-lite",
        PricingStructure::Flat {
            input_per_1m: 0.10,
            output_per_1m: 0.40
        },
        CachingSupport::None,
        true
    );

    // StepFun Models
    // StepFun publishes CNY rates only (0.70 / 0.14 / 2.10 per 1M tokens), so
    // these use the 7 CNY-per-USD conversion established for this provider.
    // Source: https://platform.stepfun.com/docs/zh/guides/pricing/details
    add_model!(
        "step-3.5-flash",
        PricingStructure::Flat {
            input_per_1m: 0.10,
            output_per_1m: 0.30
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.02
        },
        false
    );
    // StepFun publishes CNY rates only (1.35 / 0.27 / 8.10 per 1M tokens), so
    // these follow the same 7 CNY-per-USD conversion as `step-3.5-flash`.
    add_model!(
        "step-3.7-flash",
        PricingStructure::Flat {
            input_per_1m: 0.193,
            output_per_1m: 1.157
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.039
        },
        false
    );

    // Upstage Models
    // Source: https://www.upstage.ai/pricing/api
    add_model!(
        "solar-pro-3",
        PricingStructure::Flat {
            input_per_1m: 0.15,
            output_per_1m: 0.60
        },
        CachingSupport::OpenAI {
            cached_input_per_1m: 0.015
        },
        false
    );

    // OpenRouter Models
    // Source: unavailable (cloaked model; OpenRouter publishes no price)
    add_model!(
        "aurora-alpha",
        PricingStructure::Flat {
            input_per_1m: 0.0,
            output_per_1m: 0.0
        },
        CachingSupport::None,
        false
    );
    // OpenRouter router labels
    // Auto Router has no standalone per-token price; usage is billed at the routed model's rate.
    // Keep a zero-cost estimated placeholder so historical logs with only `auto` do not warn.
    // Source: https://openrouter.ai/docs/guides/routing/routers/auto-router
    add_model!(
        "auto",
        PricingStructure::Flat {
            input_per_1m: 0.0,
            output_per_1m: 0.0
        },
        CachingSupport::None,
        true
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
    add_alias!("gpt-5.4", "gpt-5.4");
    add_alias!("gpt-5.4-pro", "gpt-5.4-pro");
    add_alias!("gpt-5.4-mini", "gpt-5.4-mini");
    add_alias!("gpt-5.4-nano", "gpt-5.4-nano");
    add_alias!("gpt-5.5", "gpt-5.5");
    add_alias!("gpt-5.5-2026-04-23", "gpt-5.5");
    add_alias!("gpt-5.5-pro", "gpt-5.5-pro");
    add_alias!("gpt-6", "gpt-6-astra");
    add_alias!("gpt-6-astra", "gpt-6-astra");
    add_alias!("gpt-5.6", "gpt-5.6-sol");
    add_alias!("gpt-5.6-sol", "gpt-5.6-sol");
    add_alias!("gpt-5.6-sol-ultra", "gpt-5.6-sol");
    add_alias!("gpt-5.6-terra", "gpt-5.6-terra");
    add_alias!("gpt-5.6-luna", "gpt-5.6-luna");
    add_alias!("openai.gpt-oss-safeguard-120b", "gpt-oss-safeguard-120b");

    // Anthropic aliases
    add_alias!("claude-fable-5-1", "claude-fable-5-1");
    add_alias!("claude-fable-5.1", "claude-fable-5-1");
    add_alias!("claude-5.1-fable", "claude-fable-5-1");
    add_alias!("anthropic.claude-fable-5-1", "claude-fable-5-1");
    add_alias!("claude-fable-5", "claude-fable-5");
    add_alias!("claude-fable-5.0", "claude-fable-5");
    add_alias!("claude-5-fable", "claude-fable-5");
    add_alias!("claude-5.0-fable", "claude-fable-5");
    add_alias!("claude-sonnet-5", "claude-sonnet-5");
    add_alias!("claude-sonnet-5.0", "claude-sonnet-5");
    add_alias!("claude-5-sonnet", "claude-sonnet-5");
    add_alias!("claude-5.0-sonnet", "claude-sonnet-5");
    add_alias!("global.anthropic.claude-sonnet-5", "claude-sonnet-5");
    add_alias!("claude-opus-5", "claude-opus-5");
    add_alias!("claude-opus-5.0", "claude-opus-5");
    add_alias!("claude-5-opus", "claude-opus-5");
    add_alias!("claude-5.0-opus", "claude-opus-5");
    add_alias!("global.anthropic.claude-opus-5", "claude-opus-5");
    add_alias!("claude-opus-4.8", "claude-opus-4-8");
    add_alias!("claude-4.8-opus", "claude-opus-4-8");
    add_alias!("claude-opus-4-8", "claude-opus-4-8");
    add_alias!("claude-opus-4.7", "claude-opus-4-7");
    add_alias!("claude-opus-4.6", "claude-opus-4-6");
    add_alias!("claude-4.6-opus", "claude-opus-4-6");
    add_alias!("claude-4.6-opus-20260205", "claude-opus-4-6");
    add_alias!("eu.anthropic.claude-opus-4-6-v1", "claude-opus-4-6");
    add_alias!("claude-opus-4-6", "claude-opus-4-6");
    add_alias!("claude-opus-4-5", "claude-opus-4-5");
    add_alias!("claude-opus-4.5", "claude-opus-4-5");
    add_alias!("claude-opus-4-5-20251101", "claude-opus-4-5");
    add_alias!("claude-opus-4", "claude-opus-4");
    add_alias!("claude-opus-4-20250514", "claude-opus-4");
    add_alias!("us.anthropic.claude-opus-4-20250514-v1:0", "claude-opus-4");
    add_alias!("claude-opus-4-0", "claude-opus-4");
    add_alias!("claude-opus-4.1", "claude-opus-4-1");
    add_alias!("claude-opus-4-1-20250805", "claude-opus-4-1");
    add_alias!("claude-sonnet-4", "claude-sonnet-4");
    add_alias!("claude-sonnet-4-20250514", "claude-sonnet-4");
    add_alias!("claude-sonnet-4-0", "claude-sonnet-4");
    add_alias!("claude-sonnet-4.6", "claude-sonnet-4-6");
    add_alias!("global.anthropic.claude-sonnet-4-6", "claude-sonnet-4-6");
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
    add_alias!("gemini-3-flash-a", "gemini-3-flash-preview");
    add_alias!("gemini-3.8-flash", "gemini-3.8-flash");
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
    add_alias!("zai.glm-5", "glm-5");
    add_alias!("glm-5-20260211", "glm-5");
    add_alias!("glm-5.1", "glm-5.1");
    add_alias!("zai.glm-5.1", "glm-5.1");
    add_alias!("glm-5-code", "glm-5-code");
    add_alias!("glm-5-code-20260211", "glm-5-code");
    add_alias!("glm-4.5-air-20260211", "glm-4.5-air");
    add_alias!("zai.glm-5.3", "glm-5.3");
    add_alias!("zai-glm-5.3", "glm-5.3");
    add_alias!("zai.glm-5.3-flash", "glm-5.3-flash");
    add_alias!("zai-glm-5.3-flash", "glm-5.3-flash");
    add_alias!("zai.glm-5.2", "glm-5.2");
    add_alias!("zai-glm-5.2", "glm-5.2");

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
    add_alias!("minimax-m2.7", "minimax-m2.7");
    add_alias!("minimax-m3", "minimax-m3");

    // DeepSeek aliases
    add_alias!("deepseek-flash", "deepseek-v4-flash");

    // Moonshot / ByteDance / Qwen / Xiaomi / Meituan aliases
    add_alias!("doubao-seed-code", "doubao-seed-2.0-code");
    add_alias!("kimi-k3", "kimi-k3");
    add_alias!("moonshotai.kimi-k3", "kimi-k3");
    add_alias!("kimi-k2.7-code", "kimi-k2.7-code");
    add_alias!("moonshotai.kimi-k2.7-code", "kimi-k2.7-code");
    add_alias!("kimi-k2.7-code-highspeed", "kimi-k2.7-code-highspeed");
    add_alias!(
        "moonshotai.kimi-k2.7-code-highspeed",
        "kimi-k2.7-code-highspeed"
    );
    add_alias!("kimi-k2.6", "kimi-k2.6");
    add_alias!("moonshotai.kimi-k2.6", "kimi-k2.6");
    add_alias!("kimi-k2.5", "kimi-k2.5");
    add_alias!("moonshotai.kimi-k2.5", "kimi-k2.5");
    add_alias!("qwen3.6-plus", "qwen3.6-plus");
    add_alias!("qwen.qwen3.6-plus", "qwen3.6-plus");
    add_alias!("qwen3.7-plus", "qwen3.7-plus");
    add_alias!("qwen.qwen3.7-plus", "qwen3.7-plus");
    add_alias!("qwen3.7-flash", "qwen3.7-flash");
    add_alias!("qwen.qwen3.7-flash", "qwen3.7-flash");
    add_alias!("qwen3.8-max", "qwen3.8-max");
    add_alias!("qwen.qwen3.8-max", "qwen3.8-max");
    add_alias!("qwen3.7-max", "qwen3.7-max");
    add_alias!("qwen.qwen3.7-max", "qwen3.7-max");
    add_alias!("mimo-v2.5-pro", "mimo-v2.5-pro");
    add_alias!("xiaomi.mimo-v2.5-pro", "mimo-v2.5-pro");
    add_alias!("mimo-v2-omni", "mimo-v2-omni");
    add_alias!("xiaomi.mimo-v2-omni", "mimo-v2-omni");

    // StepFun aliases
    add_alias!("step-3.5-flash", "step-3.5-flash");
    add_alias!("step-3.7-flash", "step-3.7-flash");
    add_alias!("stepfun.step-3.7-flash", "step-3.7-flash");

    // Upstage aliases
    add_alias!("solar-pro-3", "solar-pro-3");

    // Aurora aliases
    add_alias!("aurora-alpha", "aurora-alpha");

    // xAI aliases
    add_alias!("grok-code-fast-1", "grok-build-0.1");
    add_alias!("grok-code-fast", "grok-build-0.1");
    add_alias!("grok-code-fast-1-0825", "grok-build-0.1");
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
            service_tiers: HashMap::new(),
            dated_pricing: Vec::new(),
            time_of_day_pricing: None,
            input_token_semantics: InputTokenSemantics::ExcludesCache,
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

fn dated_pricing_for_date(
    model_info: &ModelInfo,
    effective_at: Option<DateTime<Utc>>,
) -> Option<&DatedPricing> {
    let usage_date = effective_at?.date_naive();
    // Pick the *nearest* matching override rather than the first one in
    // the vector. Built-in models are kept sorted by `add_dated_pricing!`,
    // but externally-merged `ModelInfo.dated_pricing` (from
    // `Registry::merge`) is only validated, not sorted, so `.find(...)`
    // could otherwise pick the wrong override depending on input order.
    model_info
        .dated_pricing
        .iter()
        .filter(|dated| usage_date < dated.valid_until)
        .min_by_key(|dated| dated.valid_until)
}

/// Pricing and caching selected for a specific usage instant.
///
/// `pricing` and `caching` are borrowed from whichever dated or service-tier
/// override applies; `multiplier` carries the peak/off-peak adjustment that
/// applies at that instant and scales every token category.
struct ResolvedPricing<'a> {
    pricing: &'a PricingStructure,
    caching: &'a CachingSupport,
    multiplier: f64,
}

/// Resolve the timezone a schedule's windows are expressed in.
fn pricing_timezone(schedule: &TimeOfDayPricing) -> chrono_tz::Tz {
    schedule.timezone.parse().unwrap_or_else(|_| {
        warn_once(format!(
            "WARNING: unknown pricing timezone `{}`. Defaulting to UTC.",
            schedule.timezone
        ));
        chrono_tz::UTC
    })
}

/// Multiplier applied to a model's base rates at `effective_at`.
///
/// Returns `1.0` (peak rates) when no schedule is configured or the usage
/// instant is unknown, so time-of-day pricing stays strictly opt-in and never
/// silently discounts usage whose timestamp was not recovered.
fn off_peak_multiplier(
    schedule: Option<&TimeOfDayPricing>,
    effective_at: Option<DateTime<Utc>>,
) -> f64 {
    let (Some(schedule), Some(effective_at)) = (schedule, effective_at) else {
        return 1.0;
    };

    let local = effective_at.with_timezone(&pricing_timezone(schedule));

    if schedule
        .peak_windows
        .iter()
        .any(|window| window.matches(&local))
    {
        1.0
    } else {
        schedule.off_peak_multiplier
    }
}

fn standard_pricing_for_date(
    model_info: &ModelInfo,
    effective_at: Option<DateTime<Utc>>,
) -> ResolvedPricing<'_> {
    let dated = dated_pricing_for_date(model_info, effective_at);
    let (pricing, caching) = dated
        .map(|dated| (&dated.pricing, &dated.caching))
        .unwrap_or((&model_info.pricing, &model_info.caching));
    let schedule = dated
        .and_then(|dated| dated.time_of_day_pricing.as_ref())
        .or(model_info.time_of_day_pricing.as_ref());

    ResolvedPricing {
        pricing,
        caching,
        multiplier: off_peak_multiplier(schedule, effective_at),
    }
}

fn pricing_for_service_tier(
    model_info: &ModelInfo,
    service_tier: ServiceTier,
    effective_at: Option<DateTime<Utc>>,
) -> ResolvedPricing<'_> {
    let standard = standard_pricing_for_date(model_info, effective_at);

    if service_tier == ServiceTier::Standard {
        return standard;
    }

    // Service tiers change the rate card, not the time-of-day discount, so the
    // standard multiplier carries over to the override.
    dated_pricing_for_date(model_info, effective_at)
        .and_then(|dated| dated.service_tiers.get(&service_tier))
        .or_else(|| model_info.service_tiers.get(&service_tier))
        .map(|tier| ResolvedPricing {
            pricing: &tier.pricing,
            caching: &tier.caching,
            multiplier: standard.multiplier,
        })
        .unwrap_or(standard)
}

fn input_cost_for_pricing(pricing: &PricingStructure, input_tokens: u64) -> f64 {
    match pricing {
        PricingStructure::Flat { input_per_1m, .. } => {
            (input_tokens as f64 / 1_000_000.0) * input_per_1m
        }
        PricingStructure::Tiered(tiered) => {
            calculate_tiered_cost(input_tokens, &tiered.tiers, tiered.bracket_pricing, true)
        }
    }
}

/// Calculate cost for input tokens using the model's standard pricing structure.
#[allow(dead_code)]
pub fn calculate_input_cost(model_name: &str, input_tokens: u64) -> f64 {
    calculate_input_cost_for_service_tier(model_name, ServiceTier::Standard, input_tokens)
}

/// Calculate cost for input tokens using the requested service tier.
#[allow(dead_code)]
pub fn calculate_input_cost_for_service_tier(
    model_name: &str,
    service_tier: ServiceTier,
    input_tokens: u64,
) -> f64 {
    calculate_input_cost_for_service_tier_at(model_name, service_tier, input_tokens, None)
}

/// Calculate cost for input tokens using the requested service tier and usage date.
pub fn calculate_input_cost_for_service_tier_at(
    model_name: &str,
    service_tier: ServiceTier,
    input_tokens: u64,
    effective_at: Option<DateTime<Utc>>,
) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => {
            let resolved = pricing_for_service_tier(&model_info, service_tier, effective_at);
            input_cost_for_pricing(resolved.pricing, input_tokens) * resolved.multiplier
        }
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            (input_tokens as f64 / 1_000_000.0) * 0.0 // $0 per 1M tokens fallback
        }
    }
}

fn output_cost_for_pricing(pricing: &PricingStructure, output_tokens: u64) -> f64 {
    match pricing {
        PricingStructure::Flat { output_per_1m, .. } => {
            (output_tokens as f64 / 1_000_000.0) * output_per_1m
        }
        PricingStructure::Tiered(tiered) => {
            calculate_tiered_cost(output_tokens, &tiered.tiers, tiered.bracket_pricing, false)
        }
    }
}

/// Calculate cost for output tokens using the model's standard pricing structure.
#[allow(dead_code)]
pub fn calculate_output_cost(model_name: &str, output_tokens: u64) -> f64 {
    calculate_output_cost_for_service_tier(model_name, ServiceTier::Standard, output_tokens)
}

/// Calculate cost for output tokens using the requested service tier.
#[allow(dead_code)]
pub fn calculate_output_cost_for_service_tier(
    model_name: &str,
    service_tier: ServiceTier,
    output_tokens: u64,
) -> f64 {
    calculate_output_cost_for_service_tier_at(model_name, service_tier, output_tokens, None)
}

/// Calculate cost for output tokens using the requested service tier and usage date.
pub fn calculate_output_cost_for_service_tier_at(
    model_name: &str,
    service_tier: ServiceTier,
    output_tokens: u64,
    effective_at: Option<DateTime<Utc>>,
) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => {
            let resolved = pricing_for_service_tier(&model_info, service_tier, effective_at);
            output_cost_for_pricing(resolved.pricing, output_tokens) * resolved.multiplier
        }
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            (output_tokens as f64 / 1_000_000.0) * 0.0 // $0 per 1M tokens fallback
        }
    }
}

fn cache_cost_for_caching(
    caching: &CachingSupport,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    match caching {
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
        }
        | CachingSupport::OpenAIWithWrites {
            cache_write_per_1m,
            cache_read_per_1m,
        } => {
            let creation_cost = (cache_creation_tokens as f64 / 1_000_000.0) * cache_write_per_1m;
            let read_cost = (cache_read_tokens as f64 / 1_000_000.0) * cache_read_per_1m;
            creation_cost + read_cost
        }
        CachingSupport::Tiered(tiered) => {
            // Tiered caching models currently publish cached-read rates only;
            // cache creation tokens are intentionally not charged here.
            calculate_tiered_cache_cost(cache_read_tokens, &tiered.tiers, tiered.bracket_pricing)
        }
        CachingSupport::TieredWithWrites(tiered) => calculate_tiered_cache_cost_with_writes(
            cache_creation_tokens,
            cache_read_tokens,
            &tiered.tiers,
            tiered.bracket_pricing,
        ),
    }
}

/// Calculate cost for cached tokens using the model's standard pricing structure.
#[allow(dead_code)]
pub fn calculate_cache_cost(
    model_name: &str,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    calculate_cache_cost_for_service_tier(
        model_name,
        ServiceTier::Standard,
        cache_creation_tokens,
        cache_read_tokens,
    )
}

/// Calculate cost for cached tokens using the requested service tier.
#[allow(dead_code)]
pub fn calculate_cache_cost_for_service_tier(
    model_name: &str,
    service_tier: ServiceTier,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    calculate_cache_cost_for_service_tier_at(
        model_name,
        service_tier,
        cache_creation_tokens,
        cache_read_tokens,
        None,
    )
}

/// Calculate cost for cached tokens using the requested service tier and usage date.
pub fn calculate_cache_cost_for_service_tier_at(
    model_name: &str,
    service_tier: ServiceTier,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    effective_at: Option<DateTime<Utc>>,
) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => {
            let resolved = pricing_for_service_tier(&model_info, service_tier, effective_at);
            cache_cost_for_caching(resolved.caching, cache_creation_tokens, cache_read_tokens)
                * resolved.multiplier
        }
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            (cache_read_tokens as f64 / 1_000_000.0) * 0.0 // $0 per 1M tokens fallback
        }
    }
}

/// Calculate total cost for a model usage using the model's standard pricing structure.
#[allow(dead_code)]
pub fn calculate_total_cost(
    model_name: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    calculate_total_cost_for_service_tier(
        model_name,
        ServiceTier::Standard,
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
    )
}

/// Calculate total cost for a model usage using the requested service tier.
#[allow(dead_code)]
pub fn calculate_total_cost_for_service_tier(
    model_name: &str,
    service_tier: ServiceTier,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
) -> f64 {
    calculate_total_cost_for_service_tier_at(
        model_name,
        service_tier,
        input_tokens,
        output_tokens,
        cache_creation_tokens,
        cache_read_tokens,
        None,
    )
}

/// Calculate total cost for a model usage using the requested service tier and usage date.
pub fn calculate_total_cost_for_service_tier_at(
    model_name: &str,
    service_tier: ServiceTier,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    effective_at: Option<DateTime<Utc>>,
) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => {
            let resolved = pricing_for_service_tier(&model_info, service_tier, effective_at);
            // Token sources report disjoint billable categories here: ordinary
            // input excludes cached reads and writes. Reads and writes can
            // overlap, so their maximum reconstructs the cached portion without
            // double-counting that overlap; adding it back recovers the prompt
            // context used to select whole-request long-context brackets.
            let context_tokens =
                input_tokens.saturating_add(cache_creation_tokens.max(cache_read_tokens));
            calculate_context_cost(
                resolved.pricing,
                resolved.caching,
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                context_tokens,
            ) * resolved.multiplier
        }
        None => {
            warn_once(format!(
                "WARNING: Unknown model: {model_name}. Defaulting to $0."
            ));
            0.0
        }
    }
}

/// Calculate standard cost when a model's tiers are selected by total prompt
/// context rather than by each token category independently.
pub fn calculate_total_cost_for_context_at(
    model_name: &str,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    context_tokens: u64,
    effective_at: Option<DateTime<Utc>>,
) -> f64 {
    match get_model_info(model_name) {
        Some(model_info) => {
            let resolved = standard_pricing_for_date(&model_info, effective_at);
            calculate_context_cost(
                resolved.pricing,
                resolved.caching,
                input_tokens,
                output_tokens,
                cache_creation_tokens,
                cache_read_tokens,
                context_tokens,
            ) * resolved.multiplier
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

fn calculate_tiered_cache_cost_with_writes(
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    tiers: &[CachingTierWithWrites],
    bracket_pricing: bool,
) -> f64 {
    debug_assert!(bracket_pricing);
    let context_tokens = cache_creation_tokens.max(cache_read_tokens);

    find_tier(context_tokens, tiers, |tier| tier.max_tokens)
        .map(|tier| {
            (cache_creation_tokens as f64 / 1_000_000.0) * tier.cache_write_per_1m
                + (cache_read_tokens as f64 / 1_000_000.0) * tier.cache_read_per_1m
        })
        .unwrap_or(0.0)
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

fn calculate_context_cost(
    pricing: &PricingStructure,
    caching: &CachingSupport,
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_tokens: u64,
    cache_read_tokens: u64,
    context_tokens: u64,
) -> f64 {
    let token_cost = match pricing {
        PricingStructure::Flat {
            input_per_1m,
            output_per_1m,
        } => {
            (input_tokens as f64 / 1_000_000.0) * input_per_1m
                + (output_tokens as f64 / 1_000_000.0) * output_per_1m
        }
        PricingStructure::Tiered(tiered) => {
            find_tier(context_tokens, &tiered.tiers, |tier| tier.max_tokens)
                .map(|tier| {
                    (input_tokens as f64 / 1_000_000.0) * tier.input_per_1m
                        + (output_tokens as f64 / 1_000_000.0) * tier.output_per_1m
                })
                .unwrap_or(0.0)
        }
    };

    let cache_cost = match caching {
        CachingSupport::Tiered(tiered) => {
            // Tiered caching models currently publish cached-read rates only;
            // cache creation tokens are intentionally not charged here.
            find_tier(context_tokens, &tiered.tiers, |tier| tier.max_tokens)
                .map(|tier| (cache_read_tokens as f64 / 1_000_000.0) * tier.cached_input_per_1m)
                .unwrap_or(0.0)
        }
        CachingSupport::TieredWithWrites(tiered) => {
            find_tier(context_tokens, &tiered.tiers, |tier| tier.max_tokens)
                .map(|tier| {
                    (cache_creation_tokens as f64 / 1_000_000.0) * tier.cache_write_per_1m
                        + (cache_read_tokens as f64 / 1_000_000.0) * tier.cache_read_per_1m
                })
                .unwrap_or(0.0)
        }
        _ => cache_cost_for_caching(caching, cache_creation_tokens, cache_read_tokens),
    };

    token_cost + cache_cost
}

#[cfg(test)]
mod tests {
    use super::{
        CachingSupport, CachingTier, CachingTierWithWrites, DatedPricing, InputTokenSemantics,
        ModelInfo, PeakWindow, PricingStructure, PricingTier, Registry, ServiceTier,
        ServiceTierPricing, TieredCaching, TieredCachingWithWrites, TieredPricing,
        TimeOfDayPricing, calculate_cache_cost, calculate_cache_cost_for_service_tier,
        calculate_cache_cost_for_service_tier_at, calculate_input_cost,
        calculate_input_cost_for_service_tier, calculate_input_cost_for_service_tier_at,
        calculate_output_cost, calculate_output_cost_for_service_tier,
        calculate_output_cost_for_service_tier_at, calculate_total_cost_for_context_at,
        calculate_total_cost_for_service_tier, calculate_total_cost_for_service_tier_at, clock,
        dated_period_mut, get_model_info, get_registry_lock, init_external_models,
        push_dated_pricing,
    };

    use chrono::{DateTime, NaiveDate, TimeZone, Utc};
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    fn approx_eq(left: f64, right: f64) {
        assert!((left - right).abs() < 1e-9, "left={left}, right={right}");
    }

    fn reset_global_registry() {
        let registry = get_registry_lock();
        *registry.write() = Registry::new_with_defaults();
    }

    fn registry_test_guard() -> std::sync::MutexGuard<'static, ()> {
        static TEST_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        TEST_MUTEX
            .get_or_init(|| Mutex::new(()))
            .lock()
            .expect("registry test mutex should not be poisoned")
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
                service_tiers: HashMap::new(),
                dated_pricing: Vec::new(),
                time_of_day_pricing: None,
                input_token_semantics: InputTokenSemantics::default(),
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
        let _guard = registry_test_guard();
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
                service_tiers: HashMap::new(),
                dated_pricing: Vec::new(),
                time_of_day_pricing: None,
                input_token_semantics: InputTokenSemantics::default(),
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
                service_tiers: HashMap::new(),
                dated_pricing: Vec::new(),
                time_of_day_pricing: None,
                input_token_semantics: InputTokenSemantics::default(),
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
        let _guard = registry_test_guard();
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
                service_tiers: HashMap::new(),
                dated_pricing: Vec::new(),
                time_of_day_pricing: None,
                input_token_semantics: InputTokenSemantics::default(),
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
        let _guard = registry_test_guard();
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
                caching: CachingSupport::Tiered(TieredCaching {
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
                service_tiers: HashMap::new(),
                dated_pricing: Vec::new(),
                time_of_day_pricing: None,
                input_token_semantics: InputTokenSemantics::default(),
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
    fn external_tiered_cache_writes_reject_progressive_pricing() {
        let _guard = registry_test_guard();
        reset_global_registry();

        let mut models = HashMap::new();
        models.insert(
            "review-progressive-tiered-writes".to_string(),
            ModelInfo {
                pricing: PricingStructure::Tiered(TieredPricing {
                    tiers: vec![
                        PricingTier {
                            max_tokens: Some(100),
                            input_per_1m: 1.0,
                            output_per_1m: 2.0,
                        },
                        PricingTier {
                            max_tokens: None,
                            input_per_1m: 3.0,
                            output_per_1m: 4.0,
                        },
                    ],
                    bracket_pricing: false,
                }),
                caching: CachingSupport::TieredWithWrites(TieredCachingWithWrites {
                    tiers: vec![
                        CachingTierWithWrites {
                            max_tokens: Some(100),
                            cache_write_per_1m: 1.25,
                            cache_read_per_1m: 0.10,
                        },
                        CachingTierWithWrites {
                            max_tokens: None,
                            cache_write_per_1m: 3.75,
                            cache_read_per_1m: 0.30,
                        },
                    ],
                    bracket_pricing: true,
                }),
                service_tiers: HashMap::new(),
                dated_pricing: Vec::new(),
                time_of_day_pricing: None,
                input_token_semantics: InputTokenSemantics::default(),
                is_estimated: false,
            },
        );

        init_external_models(models, HashMap::new());

        assert!(get_model_info("review-progressive-tiered-writes").is_none());

        reset_global_registry();
    }

    #[test]
    fn claude_opus_5_alias_maps_to_pricing() {
        let model_info = get_model_info("claude-5-opus").expect("model should exist");
        assert!(!model_info.is_estimated);

        let input_cost = calculate_input_cost("claude-5-opus", 1_000_000);
        let output_cost = calculate_output_cost("claude-5-opus", 1_000_000);
        let cache_cost = calculate_cache_cost("claude-5-opus", 1_000_000, 1_000_000);

        approx_eq(input_cost, 5.0);
        approx_eq(output_cost, 25.0);
        approx_eq(cache_cost, 6.75);
    }

    #[test]
    fn claude_opus_4_8_alias_maps_to_pricing() {
        let model_info = get_model_info("claude-4.8-opus").expect("model should exist");
        assert!(!model_info.is_estimated);

        let input_cost = calculate_input_cost("claude-4.8-opus", 1_000_000);
        let output_cost = calculate_output_cost("claude-4.8-opus", 1_000_000);
        let cache_cost = calculate_cache_cost("claude-4.8-opus", 1_000_000, 1_000_000);

        approx_eq(input_cost, 5.0);
        approx_eq(output_cost, 25.0);
        approx_eq(cache_cost, 6.75);
    }

    #[test]
    fn claude_fable_5_1_aliases_map_to_official_pricing() {
        for model in [
            "claude-fable-5-1",
            "claude-fable-5.1",
            "claude-5.1-fable",
            "anthropic.claude-fable-5-1",
        ] {
            let model_info = get_model_info(model).expect("Fable 5.1 alias should resolve");
            assert!(!model_info.is_estimated);

            approx_eq(calculate_input_cost(model, 1_000_000), 10.0);
            approx_eq(calculate_output_cost(model, 1_000_000), 50.0);
            // One million default 5-minute cache writes plus one million reads
            // cost $12.50 + $0.25. This assertion locks in Fable 5.1's special
            // 0.025x read multiplier rather than Anthropic's usual 0.1x rate.
            approx_eq(calculate_cache_cost(model, 1_000_000, 1_000_000), 12.75);
        }
    }

    #[test]
    fn claude_fable_5_1_batch_tier_stacks_with_cache_discount() {
        // Anthropic applies Batch's 50% discount to base and prompt-cache
        // prices, so every token category must participate in this assertion.
        approx_eq(
            calculate_total_cost_for_service_tier_at(
                "claude-fable-5-1",
                ServiceTier::Batch,
                1_000_000,
                1_000_000,
                1_000_000,
                1_000_000,
                None,
            ),
            36.375,
        );
    }

    #[test]
    fn claude_fable_5_alias_maps_to_pricing() {
        let model_info = get_model_info("claude-5-fable").expect("model should exist");
        assert!(!model_info.is_estimated);

        let input_cost = calculate_input_cost("claude-5-fable", 1_000_000);
        let output_cost = calculate_output_cost("claude-5-fable", 1_000_000);
        let cache_cost = calculate_cache_cost("claude-5-fable", 1_000_000, 1_000_000);

        approx_eq(input_cost, 10.0);
        approx_eq(output_cost, 50.0);
        approx_eq(cache_cost, 13.5);
    }

    #[test]
    fn claude_sonnet_5_alias_maps_to_permanent_pricing() {
        let model_info = get_model_info("claude-5-sonnet").expect("model should exist");
        assert!(!model_info.is_estimated);
        assert!(model_info.dated_pricing.is_empty());

        let input_cost = calculate_input_cost("claude-5-sonnet", 1_000_000);
        let output_cost = calculate_output_cost("claude-5-sonnet", 1_000_000);
        let cache_cost = calculate_cache_cost("claude-5-sonnet", 1_000_000, 1_000_000);

        approx_eq(input_cost, 2.0);
        approx_eq(output_cost, 10.0);
        approx_eq(cache_cost, 2.7);
    }

    #[test]
    fn claude_sonnet_5_permanent_pricing_has_no_september_boundary() {
        for effective_at in [
            Utc.with_ymd_and_hms(2026, 8, 31, 23, 59, 59).unwrap(),
            Utc.with_ymd_and_hms(2026, 9, 1, 0, 0, 0).unwrap(),
        ] {
            approx_eq(
                calculate_total_cost_for_service_tier_at(
                    "claude-5-sonnet",
                    ServiceTier::Standard,
                    1_000_000,
                    1_000_000,
                    1_000_000,
                    1_000_000,
                    Some(effective_at),
                ),
                14.7,
            );
        }
    }

    #[test]
    fn claude_sonnet_5_global_anthropic_alias_maps_to_permanent_pricing() {
        let model_info = get_model_info("global.anthropic.claude-sonnet-5")
            .expect("global Anthropic alias should resolve");
        assert!(!model_info.is_estimated);
        assert!(model_info.dated_pricing.is_empty());

        approx_eq(
            calculate_input_cost("global.anthropic.claude-sonnet-5", 1_000_000),
            2.0,
        );
        approx_eq(
            calculate_output_cost("global.anthropic.claude-sonnet-5", 1_000_000),
            10.0,
        );
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
    fn gemini_2_5_cache_reads_match_published_rates() {
        // ai.google.dev lists context caching at 10% of input: $0.125/$0.25 for
        // Pro, $0.03 for Flash, and $0.01 for Flash-Lite.
        approx_eq(calculate_cache_cost("gemini-2.5-pro", 0, 100_000), 0.0125);
        // Pro applies the tiers progressively rather than bracketing the whole
        // request: 200k at $0.125 plus 50k at $0.25.
        approx_eq(calculate_cache_cost("gemini-2.5-pro", 0, 250_000), 0.0375);
        approx_eq(calculate_cache_cost("gemini-2.5-flash", 0, 1_000_000), 0.03);
        approx_eq(
            calculate_cache_cost("gemini-2.5-flash-lite", 0, 1_000_000),
            0.01,
        );
    }

    #[test]
    fn minimax_m2_5_pricing_matches_published_rates() {
        let model_info = get_model_info("minimax-m2.5").expect("model should exist");
        assert!(!model_info.is_estimated);

        approx_eq(calculate_input_cost("minimax-m2.5", 1_000_000), 0.30);
        approx_eq(calculate_output_cost("minimax-m2.5", 1_000_000), 1.20);
        // Pay-as-you-go lists cache reads at $0.03 and cache writes at $0.375
        // for the legacy M2 era, matching the M2.7 entry.
        approx_eq(calculate_cache_cost("minimax-m2.5", 0, 1_000_000), 0.03);
        approx_eq(calculate_cache_cost("minimax-m2.5", 1_000_000, 0), 0.375);
        approx_eq(calculate_cache_cost("minimax-m2.1", 0, 1_000_000), 0.03);
    }

    #[test]
    fn gpt_6_astra_aliases_map_to_official_standard_pricing() {
        assert!(get_model_info("gpt-6-astra-2026-09-03").is_none());

        for model in ["gpt-6-astra", "gpt-6"] {
            let model_info = get_model_info(model).expect("GPT-6 Astra alias should resolve");
            assert!(!model_info.is_estimated);

            approx_eq(calculate_input_cost(model, 200_000), 2.0);
            approx_eq(calculate_output_cost(model, 200_000), 10.0);
            // Cache-only helpers select their bracket from the cache counts,
            // so exercise both published tiers directly: 200K writes plus
            // reads cost $2.50 + $0.20; one million of each costs $25 + $2.
            approx_eq(calculate_cache_cost(model, 200_000, 200_000), 2.70);
            approx_eq(calculate_cache_cost(model, 1_000_000, 1_000_000), 27.0);
        }
    }

    #[test]
    fn gpt_6_astra_context_boundary_selects_one_rate_for_every_token_category() {
        for (input, expected) in [(172_000, 2.32), (172_001, 4.390_02)] {
            let cost = calculate_total_cost_for_service_tier_at(
                "gpt-6-astra",
                ServiceTier::Standard,
                input,
                10_000,
                0,
                100_000,
                None,
            );
            // Crossing 272K total prompt tokens changes the bracket for the
            // entire request, including output and cached input; it does not
            // progressively surcharge only the 272,001st input token.
            approx_eq(cost, expected);
        }
    }

    #[test]
    fn gpt_6_astra_service_tiers_preserve_long_context_multipliers() {
        for (service_tier, expected_short, expected_long) in [
            (ServiceTier::Standard, 2.32, 4.390_002),
            (ServiceTier::Priority, 4.64, 8.780_004),
            (ServiceTier::Flex, 1.16, 2.195_001),
            (ServiceTier::Batch, 1.16, 2.195_001),
        ] {
            let short = calculate_total_cost_for_service_tier_at(
                "gpt-6-astra",
                service_tier,
                172_000,
                10_000,
                0,
                100_000,
                None,
            );
            let long = calculate_total_cost_for_service_tier_at(
                "gpt-6-astra",
                service_tier,
                172_000,
                10_000,
                0,
                100_001,
                None,
            );

            approx_eq(short, expected_short);
            approx_eq(long, expected_long);
        }
    }

    #[test]
    fn gpt_5_4_uses_long_context_pricing_for_full_session() {
        let input_cost = calculate_input_cost("gpt-5.4", 1_000_000);
        let output_cost = calculate_output_cost("gpt-5.4", 1_000_000);
        let cache_cost = calculate_cache_cost("gpt-5.4", 0, 1_000_000);

        approx_eq(input_cost, 5.0);
        approx_eq(output_cost, 22.5);
        approx_eq(cache_cost, 0.50);
    }

    #[test]
    fn gpt_5_4_pro_uses_long_context_pricing_for_full_session() {
        let input_cost = calculate_input_cost("gpt-5.4-pro", 1_000_000);
        let output_cost = calculate_output_cost("gpt-5.4-pro", 1_000_000);

        approx_eq(input_cost, 60.0);
        approx_eq(output_cost, 270.0);
    }

    #[test]
    fn gpt_5_5_uses_long_context_pricing_for_full_session() {
        let input_cost = calculate_input_cost("gpt-5.5", 1_000_000);
        let output_cost = calculate_output_cost("gpt-5.5", 1_000_000);
        let cache_cost = calculate_cache_cost("gpt-5.5", 0, 1_000_000);

        approx_eq(input_cost, 10.0);
        approx_eq(output_cost, 45.0);
        approx_eq(cache_cost, 1.0);
    }

    #[test]
    fn gpt_5_6_sol_uses_long_context_pricing_for_full_request() {
        let cost = calculate_total_cost_for_service_tier_at(
            "gpt-5.6-sol",
            ServiceTier::Standard,
            100_000,
            10_000,
            0,
            200_000,
            None,
        );

        // The 300K prompt crosses the 272K boundary even though uncached input,
        // cached input, and output are each below it individually.
        approx_eq(cost, 1.26);
    }

    #[test]
    fn gpt_5_6_sol_context_boundary_selects_one_rate_for_every_token_category() {
        for (input, expected) in [(172_000, 0.928), (172_001, 1.756_008)] {
            let cost = calculate_total_cost_for_service_tier_at(
                "gpt-5.6-sol",
                ServiceTier::Standard,
                input,
                10_000,
                0,
                100_000,
                None,
            );
            approx_eq(cost, expected);
        }
    }

    #[test]
    fn gpt_5_6_terra_and_luna_use_long_context_pricing_for_full_request() {
        for (model, expected) in [("gpt-5.6-terra", 0.66), ("gpt-5.6-luna", 0.066)] {
            let cost = calculate_total_cost_for_service_tier_at(
                model,
                ServiceTier::Standard,
                100_000,
                10_000,
                0,
                200_000,
                None,
            );
            approx_eq(cost, expected);
        }
    }

    #[test]
    fn gpt_5_6_pricing_is_available() {
        let sol_info = get_model_info("gpt-5.6-sol").expect("model should exist");
        let terra_info = get_model_info("gpt-5.6-terra").expect("model should exist");
        let luna_info = get_model_info("gpt-5.6-luna").expect("model should exist");
        assert!(!sol_info.is_estimated);
        assert!(!terra_info.is_estimated);
        assert!(!luna_info.is_estimated);

        approx_eq(calculate_input_cost("gpt-5.6-sol", 200_000), 0.8);
        approx_eq(calculate_output_cost("gpt-5.6-sol", 200_000), 4.0);
        approx_eq(calculate_cache_cost("gpt-5.6-sol", 0, 200_000), 0.08);
        approx_eq(calculate_cache_cost("gpt-5.6-sol", 100_000, 100_000), 0.54);

        approx_eq(calculate_input_cost("gpt-5.6-terra", 1_000_000), 4.0);
        approx_eq(calculate_output_cost("gpt-5.6-terra", 1_000_000), 18.0);
        approx_eq(calculate_cache_cost("gpt-5.6-terra", 0, 1_000_000), 0.40);
        approx_eq(
            calculate_cache_cost("gpt-5.6-terra", 1_000_000, 1_000_000),
            5.40,
        );

        approx_eq(calculate_input_cost("gpt-5.6-luna", 1_000_000), 0.40);
        approx_eq(calculate_output_cost("gpt-5.6-luna", 1_000_000), 1.80);
        approx_eq(calculate_cache_cost("gpt-5.6-luna", 0, 1_000_000), 0.04);
        approx_eq(
            calculate_cache_cost("gpt-5.6-luna", 1_000_000, 1_000_000),
            0.54,
        );
    }

    /// Usage from before the promotion must retain Sol's original sticker price
    /// and long-context multiplier across every service tier.
    #[test]
    fn gpt_5_6_sol_keeps_pre_promotion_pricing_for_older_usage() {
        let before_promotion = Utc.with_ymd_and_hms(2026, 8, 20, 23, 59, 59).unwrap();

        for (service_tier, expected) in [
            (ServiceTier::Standard, 1.65),
            (ServiceTier::Priority, 3.30),
            (ServiceTier::Flex, 0.825),
            (ServiceTier::Batch, 0.825),
        ] {
            let cost = calculate_total_cost_for_service_tier_at(
                "gpt-5.6-sol",
                service_tier,
                100_000,
                10_000,
                0,
                200_000,
                Some(before_promotion),
            );
            approx_eq(cost, expected);
        }
    }

    /// The promotion was first published on 2026-08-21, so that UTC day is
    /// already billed at the promotional rates while earlier usage is unchanged.
    #[test]
    fn gpt_5_6_sol_uses_promotional_pricing_from_publication_date() {
        let promotion_day = Utc.with_ymd_and_hms(2026, 8, 21, 0, 0, 0).unwrap();

        for (service_tier, input, output, cache_read, cache_write) in [
            (ServiceTier::Standard, 8.0, 30.0, 0.80, 10.0),
            (ServiceTier::Priority, 16.0, 60.0, 1.60, 20.0),
            (ServiceTier::Flex, 4.0, 15.0, 0.40, 5.0),
            (ServiceTier::Batch, 4.0, 15.0, 0.40, 5.0),
        ] {
            approx_eq(
                calculate_input_cost_for_service_tier_at(
                    "gpt-5.6-sol",
                    service_tier,
                    1_000_000,
                    Some(promotion_day),
                ),
                input,
            );
            approx_eq(
                calculate_output_cost_for_service_tier_at(
                    "gpt-5.6-sol",
                    service_tier,
                    1_000_000,
                    Some(promotion_day),
                ),
                output,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier_at(
                    "gpt-5.6-sol",
                    service_tier,
                    0,
                    1_000_000,
                    Some(promotion_day),
                ),
                cache_read,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier_at(
                    "gpt-5.6-sol",
                    service_tier,
                    1_000_000,
                    0,
                    Some(promotion_day),
                ),
                cache_write,
            );
        }
    }

    /// Usage from before the 2026-07-30 cut must keep both the historical
    /// sticker price and its long-context multiplier across every service tier.
    #[test]
    fn gpt_5_6_terra_and_luna_keep_pre_cut_pricing_for_older_usage() {
        let before_cut = Utc.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap();

        for (service_tier, terra, luna) in [
            (ServiceTier::Standard, 0.825, 0.33),
            (ServiceTier::Priority, 1.65, 0.66),
            (ServiceTier::Flex, 0.4125, 0.165),
            (ServiceTier::Batch, 0.4125, 0.165),
        ] {
            for (model, expected) in [("gpt-5.6-terra", terra), ("gpt-5.6-luna", luna)] {
                let cost = calculate_total_cost_for_service_tier_at(
                    model,
                    service_tier,
                    100_000,
                    10_000,
                    0,
                    200_000,
                    Some(before_cut),
                );
                approx_eq(cost, expected);
            }
        }
    }

    /// The cut took effect on 2026-07-30, so that day is already billed at the
    /// new rates: `standard_pricing_for_date` treats `valid_until` as exclusive.
    #[test]
    fn gpt_5_6_terra_and_luna_use_new_pricing_from_the_cut_date() {
        let cut_day = Utc.with_ymd_and_hms(2026, 7, 30, 0, 0, 0).unwrap();

        for (model, input, output) in [("gpt-5.6-terra", 4.0, 18.0), ("gpt-5.6-luna", 0.40, 1.80)] {
            approx_eq(
                calculate_input_cost_for_service_tier_at(
                    model,
                    ServiceTier::Standard,
                    1_000_000,
                    Some(cut_day),
                ),
                input,
            );
            approx_eq(
                calculate_output_cost_for_service_tier_at(
                    model,
                    ServiceTier::Standard,
                    1_000_000,
                    Some(cut_day),
                ),
                output,
            );
        }
    }

    #[test]
    fn gpt_5_6_aliases_map_to_sol_pricing() {
        let model_info = get_model_info("gpt-5.6-sol-ultra").expect("alias should resolve");
        assert!(!model_info.is_estimated);

        approx_eq(calculate_input_cost("gpt-5.6", 1_000_000), 8.0);
        approx_eq(calculate_output_cost("gpt-5.6", 1_000_000), 30.0);
        approx_eq(
            calculate_cache_cost("gpt-5.6-sol-ultra", 0, 1_000_000),
            0.80,
        );
    }

    #[test]
    fn gpt_priority_pricing_is_available_for_supported_models() {
        approx_eq(
            calculate_input_cost_for_service_tier("gpt-5.6-sol", ServiceTier::Priority, 1_000_000),
            16.0,
        );
        approx_eq(
            calculate_output_cost_for_service_tier("gpt-5.6-sol", ServiceTier::Priority, 1_000_000),
            60.0,
        );
        approx_eq(
            calculate_cache_cost_for_service_tier(
                "gpt-5.6-sol",
                ServiceTier::Priority,
                0,
                1_000_000,
            ),
            1.60,
        );
        approx_eq(
            calculate_cache_cost_for_service_tier(
                "gpt-5.6-sol",
                ServiceTier::Priority,
                1_000_000,
                1_000_000,
            ),
            21.60,
        );

        approx_eq(
            calculate_input_cost_for_service_tier(
                "gpt-5.6-terra",
                ServiceTier::Priority,
                1_000_000,
            ),
            8.0,
        );
        approx_eq(
            calculate_output_cost_for_service_tier(
                "gpt-5.6-terra",
                ServiceTier::Priority,
                1_000_000,
            ),
            36.0,
        );
        approx_eq(
            calculate_cache_cost_for_service_tier(
                "gpt-5.6-terra",
                ServiceTier::Priority,
                0,
                1_000_000,
            ),
            0.80,
        );
        approx_eq(
            calculate_cache_cost_for_service_tier(
                "gpt-5.6-terra",
                ServiceTier::Priority,
                1_000_000,
                1_000_000,
            ),
            10.80,
        );

        approx_eq(
            calculate_input_cost_for_service_tier("gpt-5.6-luna", ServiceTier::Priority, 1_000_000),
            0.80,
        );
        approx_eq(
            calculate_output_cost_for_service_tier(
                "gpt-5.6-luna",
                ServiceTier::Priority,
                1_000_000,
            ),
            3.60,
        );
        approx_eq(
            calculate_cache_cost_for_service_tier(
                "gpt-5.6-luna",
                ServiceTier::Priority,
                0,
                1_000_000,
            ),
            0.08,
        );
        approx_eq(
            calculate_cache_cost_for_service_tier(
                "gpt-5.6-luna",
                ServiceTier::Priority,
                1_000_000,
                1_000_000,
            ),
            1.08,
        );

        approx_eq(
            calculate_input_cost_for_service_tier("gpt-5.5", ServiceTier::Priority, 1_000_000),
            12.50,
        );
        approx_eq(
            calculate_output_cost_for_service_tier("gpt-5.5", ServiceTier::Priority, 1_000_000),
            75.0,
        );
        approx_eq(
            calculate_cache_cost_for_service_tier("gpt-5.5", ServiceTier::Priority, 0, 1_000_000),
            1.25,
        );

        approx_eq(
            calculate_input_cost_for_service_tier("gpt-5.4", ServiceTier::Priority, 1_000_000),
            5.0,
        );
        approx_eq(
            calculate_output_cost_for_service_tier("gpt-5.4", ServiceTier::Priority, 1_000_000),
            30.0,
        );
        approx_eq(
            calculate_cache_cost_for_service_tier("gpt-5.4", ServiceTier::Priority, 0, 1_000_000),
            0.50,
        );

        approx_eq(
            calculate_input_cost_for_service_tier("gpt-5.4-mini", ServiceTier::Priority, 1_000_000),
            1.50,
        );
        approx_eq(
            calculate_output_cost_for_service_tier(
                "gpt-5.4-mini",
                ServiceTier::Priority,
                1_000_000,
            ),
            9.0,
        );
        approx_eq(
            calculate_cache_cost_for_service_tier(
                "gpt-5.4-mini",
                ServiceTier::Priority,
                0,
                1_000_000,
            ),
            0.15,
        );
    }

    #[test]
    fn gpt_flex_and_batch_pricing_are_available_for_supported_models() {
        for service_tier in [ServiceTier::Flex, ServiceTier::Batch] {
            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.6-sol", service_tier, 1_000_000),
                4.0,
            );
            approx_eq(
                calculate_output_cost_for_service_tier("gpt-5.6-sol", service_tier, 1_000_000),
                15.0,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier("gpt-5.6-sol", service_tier, 0, 1_000_000),
                0.40,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier(
                    "gpt-5.6-sol",
                    service_tier,
                    1_000_000,
                    1_000_000,
                ),
                5.40,
            );

            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.6-terra", service_tier, 1_000_000),
                2.0,
            );
            approx_eq(
                calculate_output_cost_for_service_tier("gpt-5.6-terra", service_tier, 1_000_000),
                9.0,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier("gpt-5.6-terra", service_tier, 0, 1_000_000),
                0.20,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier(
                    "gpt-5.6-terra",
                    service_tier,
                    1_000_000,
                    1_000_000,
                ),
                2.70,
            );

            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.6-luna", service_tier, 1_000_000),
                0.20,
            );
            approx_eq(
                calculate_output_cost_for_service_tier("gpt-5.6-luna", service_tier, 1_000_000),
                0.90,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier("gpt-5.6-luna", service_tier, 0, 1_000_000),
                0.02,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier(
                    "gpt-5.6-luna",
                    service_tier,
                    1_000_000,
                    1_000_000,
                ),
                0.27,
            );

            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.5", service_tier, 1_000_000),
                5.0,
            );
            approx_eq(
                calculate_output_cost_for_service_tier("gpt-5.5", service_tier, 1_000_000),
                22.50,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier("gpt-5.5", service_tier, 0, 1_000_000),
                0.50,
            );

            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.4", service_tier, 200_000),
                0.25,
            );
            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.4", service_tier, 1_000_000),
                2.50,
            );
            approx_eq(
                calculate_output_cost_for_service_tier("gpt-5.4", service_tier, 1_000_000),
                11.25,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier("gpt-5.4", service_tier, 0, 200_000),
                0.026,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier("gpt-5.4", service_tier, 0, 1_000_000),
                0.25,
            );

            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.5-pro", service_tier, 1_000_000),
                15.0,
            );
            approx_eq(
                calculate_output_cost_for_service_tier("gpt-5.5-pro", service_tier, 1_000_000),
                90.0,
            );

            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.4-pro", service_tier, 1_000_000),
                30.0,
            );
            approx_eq(
                calculate_output_cost_for_service_tier("gpt-5.4-pro", service_tier, 1_000_000),
                135.0,
            );

            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.4-mini", service_tier, 1_000_000),
                0.375,
            );
            approx_eq(
                calculate_output_cost_for_service_tier("gpt-5.4-mini", service_tier, 1_000_000),
                2.25,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier("gpt-5.4-mini", service_tier, 0, 1_000_000),
                0.0375,
            );

            approx_eq(
                calculate_input_cost_for_service_tier("gpt-5.4-nano", service_tier, 1_000_000),
                0.10,
            );
            approx_eq(
                calculate_output_cost_for_service_tier("gpt-5.4-nano", service_tier, 1_000_000),
                0.625,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier("gpt-5.4-nano", service_tier, 0, 1_000_000),
                0.01,
            );
        }
    }

    #[test]
    fn missing_service_tier_pricing_falls_back_to_standard() {
        let standard_cost = calculate_input_cost("gpt-5.4-nano", 1_000_000);
        let priority_cost =
            calculate_input_cost_for_service_tier("gpt-5.4-nano", ServiceTier::Priority, 1_000_000);

        approx_eq(priority_cost, standard_cost);
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
    fn xai_standard_pricing_uses_context_tiers_and_aliases() {
        assert!(
            !get_model_info("grok-4.5")
                .expect("Grok 4.5 should exist")
                .is_estimated
        );
        assert!(
            !get_model_info("grok-4.6")
                .expect("Grok 4.6 should exist")
                .is_estimated
        );
        for model in [
            "grok-4.3",
            "grok-4.20-0309-reasoning",
            "grok-4.20-0309-non-reasoning",
            "grok-4.20-multi-agent-0309",
        ] {
            assert!(
                !get_model_info(model)
                    .unwrap_or_else(|| panic!("{model} should exist"))
                    .is_estimated
            );
        }

        approx_eq(
            calculate_total_cost_for_context_at(
                "grok-4.3", 1_000_000, 1_000_000, 0, 1_000_000, 199_999, None,
            ),
            3.95,
        );
        approx_eq(
            calculate_total_cost_for_context_at(
                "grok-4.3", 1_000_000, 1_000_000, 0, 1_000_000, 200_001, None,
            ),
            7.9,
        );
        approx_eq(
            calculate_total_cost_for_context_at(
                "grok-4.5", 1_000_000, 1_000_000, 0, 1_000_000, 199_999, None,
            ),
            8.3,
        );
        approx_eq(
            calculate_total_cost_for_context_at(
                "grok-4.6", 1_000_000, 1_000_000, 0, 1_000_000, 199_999, None,
            ),
            8.5,
        );
        approx_eq(
            calculate_total_cost_for_context_at(
                "grok-4.6", 1_000_000, 1_000_000, 0, 1_000_000, 200_001, None,
            ),
            17.0,
        );
        approx_eq(
            calculate_total_cost_for_context_at(
                "grok-code-fast-1",
                1_000_000,
                1_000_000,
                0,
                1_000_000,
                200_001,
                None,
            ),
            6.4,
        );
        approx_eq(
            calculate_total_cost_for_context_at(
                "grok-4.5", 1_000_000, 1_000_000, 999_999, 1_000_000, 2_000_000, None,
            ),
            calculate_total_cost_for_context_at(
                "grok-4.5", 1_000_000, 1_000_000, 0, 1_000_000, 2_000_000, None,
            ),
        );
        approx_eq(
            calculate_total_cost_for_context_at(
                "grok-4.6", 1_000_000, 1_000_000, 999_999, 1_000_000, 2_000_000, None,
            ),
            calculate_total_cost_for_context_at(
                "grok-4.6", 1_000_000, 1_000_000, 0, 1_000_000, 2_000_000, None,
            ),
        );
    }

    #[test]
    fn doubao_seed_code_alias_resolves() {
        let model_info = get_model_info("doubao-seed-code").expect("alias should resolve");
        assert!(model_info.is_estimated);

        // Volcano Ark brackets on input length and bills every token in a
        // request at its bracket's rate, so a 1M-token request uses the top
        // bracket (9.60 / 1.92 / 48.00 CNY, converted at 7 CNY per USD).
        approx_eq(calculate_input_cost("doubao-seed-code", 1_000_000), 1.371);
        approx_eq(calculate_output_cost("doubao-seed-code", 1_000_000), 6.857);
        approx_eq(
            calculate_cache_cost("doubao-seed-code", 0, 1_000_000),
            0.274,
        );

        // A request that stays inside the first bracket uses
        // 3.20 / 0.64 / 16.00 CNY.
        approx_eq(
            calculate_input_cost("doubao-seed-code", 32_000),
            0.032 * 0.457,
        );
        approx_eq(
            calculate_output_cost("doubao-seed-code", 32_000),
            0.032 * 2.286,
        );
        approx_eq(
            calculate_cache_cost("doubao-seed-code", 0, 32_000),
            0.032 * 0.091,
        );
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
            (CachingSupport::Tiered(first_tiered), CachingSupport::Tiered(second_tiered)) => {
                assert!(
                    std::ptr::eq(first_tiered.tiers.as_ptr(), second_tiered.tiers.as_ptr()),
                    "cache tiers should not be reallocated on each lookup"
                );
            }
            _ => panic!("Expected Google caching"),
        }
    }

    #[test]
    fn mimo_v2_5_pro_pricing_is_available() {
        let model_info = get_model_info("mimo-v2.5-pro").expect("model should exist");
        assert!(model_info.is_estimated);

        let input_cost = calculate_input_cost("mimo-v2.5-pro", 1_000_000);
        let output_cost = calculate_output_cost("mimo-v2.5-pro", 1_000_000);
        let cache_cost = calculate_cache_cost("mimo-v2.5-pro", 0, 1_000_000);

        approx_eq(input_cost, 0.435);
        approx_eq(output_cost, 0.87);
        approx_eq(cache_cost, 0.0036);
    }

    #[test]
    fn deepseek_v4_pro_pricing_is_available() {
        let model_info = get_model_info("deepseek-v4-pro").expect("model should exist");
        assert!(!model_info.is_estimated);

        let input_cost = calculate_input_cost("deepseek-v4-pro", 1_000_000);
        let output_cost = calculate_output_cost("deepseek-v4-pro", 1_000_000);
        let cache_cost = calculate_cache_cost("deepseek-v4-pro", 0, 1_000_000);

        approx_eq(input_cost, 1.32);
        approx_eq(output_cost, 3.96);
        approx_eq(cache_cost, 0.044);
    }

    #[test]
    fn deepseek_flash_alias_matches_legacy_v4_flash_pricing() {
        // The page asks callers to use `deepseek-flash`; `deepseek-v4-flash`
        // is the legacy name for the same DeepSeek-V4.1-Flash model.
        for model in ["deepseek-flash", "deepseek-v4-flash"] {
            approx_eq(calculate_input_cost(model, 1_000_000), 0.30);
            approx_eq(calculate_output_cost(model, 1_000_000), 1.20);
            approx_eq(calculate_cache_cost(model, 0, 1_000_000), 0.006);
        }
    }

    #[test]
    fn qwen_3_5_35b_a3b_provider_alias_resolves() {
        let model_info =
            get_model_info("qwen/qwen3.5-35b-a3b").expect("provider-prefixed model should exist");
        assert!(model_info.is_estimated);

        let input_cost = calculate_input_cost("qwen/qwen3.5-35b-a3b", 1_000_000);
        let output_cost = calculate_output_cost("qwen/qwen3.5-35b-a3b", 1_000_000);

        approx_eq(input_cost, 0.1625);
        approx_eq(output_cost, 1.30);
    }

    #[test]
    fn longcat_flash_lite_provider_alias_resolves() {
        let model_info = get_model_info("meituan/longcat-flash-lite")
            .expect("provider-prefixed model should exist");
        assert!(model_info.is_estimated);

        let input_cost = calculate_input_cost("meituan/longcat-flash-lite", 1_000_000);
        let output_cost = calculate_output_cost("meituan/longcat-flash-lite", 1_000_000);

        approx_eq(input_cost, 0.10);
        approx_eq(output_cost, 0.40);
    }

    #[test]
    fn kimi_k2_5_pricing_is_available() {
        let model_info = get_model_info("kimi-k2.5").expect("model should exist");
        assert!(!model_info.is_estimated);

        let input_cost = calculate_input_cost("kimi-k2.5", 1_000_000);
        let output_cost = calculate_output_cost("kimi-k2.5", 1_000_000);
        let cache_cost = calculate_cache_cost("kimi-k2.5", 0, 1_000_000);

        approx_eq(input_cost, 0.60);
        approx_eq(output_cost, 3.0);
        approx_eq(cache_cost, 0.10);
    }

    #[test]
    fn warning_model_names_resolve_to_pricing() {
        let models = [
            "kimi-k2.6",
            "minimax-m2.7",
            "glm-5.1",
            "kimi-k2.5",
            "mimo-v2-omni",
            "zai.glm-5",
            "qwen3.6-plus",
            "mimo-v2.5-pro",
            "global.anthropic.claude-sonnet-4-6",
            "global.anthropic.claude-sonnet-5",
            "deepseek.v3.2",
            "moonshotai.kimi-k2.5",
            "eu.anthropic.claude-opus-4-6-v1",
            "us.anthropic.claude-opus-4-20250514-v1:0",
            "openai.gpt-oss-safeguard-120b",
            "deepseek-v4-flash",
        ];

        for model in models {
            assert!(get_model_info(model).is_some(), "{model} should resolve");
        }
    }

    #[test]
    fn newly_observed_models_have_expected_pricing() {
        approx_eq(calculate_input_cost("kimi-k2.6", 1_000_000), 0.95);
        approx_eq(calculate_output_cost("kimi-k2.6", 1_000_000), 4.0);
        approx_eq(calculate_cache_cost("kimi-k2.6", 0, 1_000_000), 0.16);

        approx_eq(calculate_input_cost("minimax-m2.7", 1_000_000), 0.30);
        approx_eq(calculate_output_cost("minimax-m2.7", 1_000_000), 1.20);
        approx_eq(
            calculate_cache_cost("minimax-m2.7", 1_000_000, 1_000_000),
            0.435,
        );

        approx_eq(calculate_input_cost("minimax-m3", 1_000_000), 0.60);
        approx_eq(calculate_output_cost("minimax-m3", 1_000_000), 2.40);
        approx_eq(calculate_cache_cost("minimax-m3", 0, 1_000_000), 0.12);
        // At or below 512k input tokens the permanent 50% discount applies.
        approx_eq(calculate_input_cost("minimax-m3", 512_000), 0.30 * 0.512);
        approx_eq(calculate_output_cost("minimax-m3", 512_000), 1.20 * 0.512);
        approx_eq(calculate_cache_cost("minimax-m3", 0, 512_000), 0.06 * 0.512);

        approx_eq(calculate_input_cost("glm-5.1", 1_000_000), 1.40);
        approx_eq(calculate_output_cost("glm-5.1", 1_000_000), 4.40);
        approx_eq(calculate_cache_cost("glm-5.1", 0, 1_000_000), 0.26);

        approx_eq(calculate_input_cost("qwen3.6-plus", 1_000_000), 2.0);
        approx_eq(calculate_output_cost("qwen3.6-plus", 1_000_000), 6.0);
        approx_eq(
            calculate_cache_cost("qwen3.6-plus", 1_000_000, 1_000_000),
            0.675,
        );

        approx_eq(calculate_input_cost("mimo-v2-omni", 1_000_000), 0.40);
        approx_eq(calculate_output_cost("mimo-v2-omni", 1_000_000), 2.0);
        approx_eq(calculate_cache_cost("mimo-v2-omni", 0, 1_000_000), 0.08);

        approx_eq(calculate_input_cost("deepseek.v3.2", 1_000_000), 0.62);
        approx_eq(calculate_output_cost("deepseek.v3.2", 1_000_000), 1.85);

        approx_eq(calculate_input_cost("deepseek-v4-flash", 1_000_000), 0.30);
        approx_eq(calculate_output_cost("deepseek-v4-flash", 1_000_000), 1.20);
        approx_eq(
            calculate_cache_cost("deepseek-v4-flash", 0, 1_000_000),
            0.006,
        );

        approx_eq(
            calculate_input_cost("openai.gpt-oss-safeguard-120b", 1_000_000),
            0.15,
        );
        approx_eq(
            calculate_output_cost("openai.gpt-oss-safeguard-120b", 1_000_000),
            0.60,
        );

        approx_eq(calculate_input_cost("glm-5.3", 1_000_000), 1.40);
        approx_eq(calculate_output_cost("glm-5.3", 1_000_000), 4.40);
        approx_eq(calculate_cache_cost("glm-5.3", 0, 1_000_000), 0.26);

        approx_eq(calculate_input_cost("glm-5.3-flash", 1_000_000), 0.15);
        approx_eq(calculate_output_cost("glm-5.3-flash", 1_000_000), 0.50);
        approx_eq(calculate_cache_cost("glm-5.3-flash", 0, 1_000_000), 0.03);

        approx_eq(calculate_input_cost("glm-5.2", 1_000_000), 1.40);
        approx_eq(calculate_output_cost("glm-5.2", 1_000_000), 4.40);
        approx_eq(calculate_cache_cost("glm-5.2", 0, 1_000_000), 0.26);

        approx_eq(calculate_input_cost("glm-4.7-flashx", 1_000_000), 0.07);
        approx_eq(calculate_output_cost("glm-4.7-flashx", 1_000_000), 0.40);
        approx_eq(calculate_cache_cost("glm-4.7-flashx", 0, 1_000_000), 0.01);

        approx_eq(calculate_input_cost("glm-4.6v-flashx", 1_000_000), 0.04);
        approx_eq(calculate_output_cost("glm-4.6v-flashx", 1_000_000), 0.40);
        approx_eq(calculate_cache_cost("glm-4.6v-flashx", 0, 1_000_000), 0.004);

        // GLM-OCR has no cache tier, so cache reads must cost nothing.
        approx_eq(calculate_input_cost("glm-ocr", 1_000_000), 0.03);
        approx_eq(calculate_output_cost("glm-ocr", 1_000_000), 0.03);
        approx_eq(calculate_cache_cost("glm-ocr", 0, 1_000_000), 0.0);

        approx_eq(calculate_input_cost("kimi-k3", 1_000_000), 3.0);
        approx_eq(calculate_output_cost("kimi-k3", 1_000_000), 15.0);
        approx_eq(calculate_cache_cost("kimi-k3", 0, 1_000_000), 0.30);

        approx_eq(calculate_input_cost("kimi-k2.7-code", 1_000_000), 0.95);
        approx_eq(calculate_output_cost("kimi-k2.7-code", 1_000_000), 4.0);
        approx_eq(calculate_cache_cost("kimi-k2.7-code", 0, 1_000_000), 0.19);

        approx_eq(
            calculate_input_cost("kimi-k2.7-code-highspeed", 1_000_000),
            1.90,
        );
        approx_eq(
            calculate_output_cost("kimi-k2.7-code-highspeed", 1_000_000),
            8.0,
        );
        approx_eq(
            calculate_cache_cost("kimi-k2.7-code-highspeed", 0, 1_000_000),
            0.38,
        );

        approx_eq(calculate_input_cost("qwen3.8-max", 1_000_000), 2.0);
        approx_eq(calculate_output_cost("qwen3.8-max", 1_000_000), 6.0);
        approx_eq(
            calculate_cache_cost("qwen3.8-max", 1_000_000, 1_000_000),
            2.70,
        );

        approx_eq(calculate_input_cost("qwen3.7-max", 1_000_000), 2.5);
        approx_eq(calculate_output_cost("qwen3.7-max", 1_000_000), 7.5);
        approx_eq(
            calculate_cache_cost("qwen3.7-max", 1_000_000, 1_000_000),
            3.375,
        );

        approx_eq(calculate_input_cost("step-3.7-flash", 1_000_000), 0.193);
        approx_eq(calculate_output_cost("step-3.7-flash", 1_000_000), 1.157);
        approx_eq(calculate_cache_cost("step-3.7-flash", 0, 1_000_000), 0.039);
    }

    #[test]
    fn gemini_3_8_flash_switches_from_promotional_to_standard_rates() {
        let promo = utc(2026, 12, 31, 23, 0);
        let standard = utc(2027, 1, 1, 0, 0);

        for (instant, input, output, cached) in
            [(promo, 0.75, 3.75, 0.075), (standard, 1.50, 7.50, 0.15)]
        {
            approx_eq(
                calculate_input_cost_for_service_tier_at(
                    "gemini-3.8-flash",
                    ServiceTier::Standard,
                    1_000_000,
                    Some(instant),
                ),
                input,
            );
            approx_eq(
                calculate_output_cost_for_service_tier_at(
                    "gemini-3.8-flash",
                    ServiceTier::Standard,
                    1_000_000,
                    Some(instant),
                ),
                output,
            );
            approx_eq(
                calculate_cache_cost_for_service_tier_at(
                    "gemini-3.8-flash",
                    ServiceTier::Standard,
                    0,
                    1_000_000,
                    Some(instant),
                ),
                cached,
            );
        }
    }

    #[test]
    fn provider_prefixed_aliases_resolve_for_new_models() {
        for name in [
            "zai.glm-5.3",
            "zai-glm-5.3",
            "zai.glm-5.3-flash",
            "zai-glm-5.3-flash",
            "zai.glm-5.2",
            "moonshotai.kimi-k3",
            "moonshotai.kimi-k2.7-code",
            "qwen.qwen3.8-max",
            "qwen.qwen3.7-max",
            "stepfun.step-3.7-flash",
            "z-ai/glm-5.3",
        ] {
            assert!(get_model_info(name).is_some(), "`{name}` should resolve");
        }
    }

    #[test]
    fn qwen_3_7_plus_pricing_and_tiers() {
        let info = get_model_info("qwen3.7-plus").expect("model should exist");
        assert!(info.is_estimated);

        // Base tier (<= 256k tokens)
        approx_eq(calculate_input_cost("qwen3.7-plus", 256_000), 0.08192);
        approx_eq(calculate_output_cost("qwen3.7-plus", 256_000), 0.32768);

        // Top tier (> 256k tokens)
        approx_eq(calculate_input_cost("qwen3.7-plus", 1_000_000), 0.96);
        approx_eq(calculate_output_cost("qwen3.7-plus", 1_000_000), 3.84);

        // Cache write + read
        approx_eq(
            calculate_cache_cost("qwen3.7-plus", 1_000_000, 1_000_000),
            0.464,
        );

        // Provider-prefixed alias resolves
        let aliased = get_model_info("qwen/qwen3.7-plus").expect("provider-prefixed alias");
        assert!(aliased.is_estimated);
    }

    #[test]
    fn qwen_3_7_flash_pricing_and_tiers() {
        let info = get_model_info("qwen3.7-flash").expect("model should exist");
        assert!(info.is_estimated);

        // First tier (<= 32k tokens)
        approx_eq(calculate_input_cost("qwen3.7-flash", 32_000), 0.00096);
        approx_eq(calculate_output_cost("qwen3.7-flash", 32_000), 0.00416);

        // Middle tier (<= 256k tokens)
        approx_eq(calculate_input_cost("qwen3.7-flash", 256_000), 0.0256);
        approx_eq(calculate_output_cost("qwen3.7-flash", 256_000), 0.1024);

        // Top tier (> 256k tokens)
        approx_eq(calculate_input_cost("qwen3.7-flash", 1_000_000), 0.20);
        approx_eq(calculate_output_cost("qwen3.7-flash", 1_000_000), 0.80);

        // Cache write + read
        approx_eq(
            calculate_cache_cost("qwen3.7-flash", 1_000_000, 1_000_000),
            0.044,
        );

        // Provider-prefixed alias resolves
        let aliased = get_model_info("qwen/qwen3.7-flash").expect("provider-prefixed alias");
        assert!(aliased.is_estimated);
    }

    #[test]
    fn bedrock_anthropic_aliases_map_to_existing_pricing() {
        approx_eq(
            calculate_input_cost("global.anthropic.claude-sonnet-4-6", 1_000_000),
            3.0,
        );
        approx_eq(
            calculate_output_cost("global.anthropic.claude-sonnet-4-6", 1_000_000),
            15.0,
        );
        approx_eq(
            calculate_input_cost("eu.anthropic.claude-opus-4-6-v1", 1_000_000),
            5.0,
        );
        approx_eq(
            calculate_output_cost("eu.anthropic.claude-opus-4-6-v1", 1_000_000),
            25.0,
        );
        approx_eq(
            calculate_input_cost("us.anthropic.claude-opus-4-20250514-v1:0", 1_000_000),
            15.0,
        );
        approx_eq(
            calculate_output_cost("us.anthropic.claude-opus-4-20250514-v1:0", 1_000_000),
            75.0,
        );
    }

    #[test]
    fn auto_router_placeholder_is_estimated_and_free() {
        let model_info = get_model_info("auto").expect("router placeholder should exist");
        assert!(model_info.is_estimated);

        let input_cost = calculate_input_cost("auto", 1_000_000);
        let output_cost = calculate_output_cost("auto", 1_000_000);
        let cache_cost = calculate_cache_cost("auto", 0, 1_000_000);

        approx_eq(input_cost, 0.0);
        approx_eq(output_cost, 0.0);
        approx_eq(cache_cost, 0.0);
    }

    /// Build a UTC instant, panicking on an invalid literal.
    fn utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(year, month, day, hour, minute, 0)
            .single()
            .expect("valid UTC instant")
    }

    fn deepseek_total_at(model: &str, effective_at: DateTime<Utc>) -> f64 {
        calculate_total_cost_for_service_tier_at(
            model,
            ServiceTier::Standard,
            1_000_000,
            1_000_000,
            0,
            1_000_000,
            Some(effective_at),
        )
    }

    #[test]
    fn deepseek_peak_windows_bill_at_full_rate() {
        // 2026-09-07 is a Monday. Peak hours are 01:00-04:00 and 06:00-10:00 UTC.
        for instant in [
            utc(2026, 9, 7, 1, 0),   // inclusive start of the first window
            utc(2026, 9, 7, 2, 30),  // inside the first window
            utc(2026, 9, 7, 8, 0),   // inside the second window
            utc(2026, 9, 11, 9, 59), // Friday, inside the second window
        ] {
            // 1M input at $1.32 + 1M output at $3.96 + 1M cache reads at $0.044
            approx_eq(deepseek_total_at("deepseek-v4-pro", instant), 5.324);
        }
    }

    #[test]
    fn deepseek_off_peak_halves_every_token_category() {
        // 2026-09-07 is a Monday.
        for instant in [
            utc(2026, 9, 7, 0, 59), // before the first window
            utc(2026, 9, 7, 4, 0),  // exclusive end of the first window
            utc(2026, 9, 7, 5, 0),  // between the two windows
            utc(2026, 9, 7, 10, 0), // exclusive end of the second window
            utc(2026, 9, 7, 23, 0), // after both windows
        ] {
            approx_eq(deepseek_total_at("deepseek-v4-pro", instant), 2.662);
        }
    }

    #[test]
    fn deepseek_weekends_are_entirely_off_peak() {
        // 2026-09-12 is a Saturday and 2026-09-13 a Sunday, so the weekday-only
        // peak windows never apply even at hours that are peak on a weekday.
        for instant in [
            utc(2026, 9, 12, 2, 0),
            utc(2026, 9, 12, 8, 0),
            utc(2026, 9, 13, 2, 0),
            utc(2026, 9, 13, 8, 0),
        ] {
            approx_eq(deepseek_total_at("deepseek-v4-pro", instant), 2.662);
        }
    }

    #[test]
    fn deepseek_flash_family_shares_the_peak_schedule() {
        // The legacy name and the current `deepseek-flash` alias resolve to the
        // same model, so both must price identically at the same instant.
        let peak = utc(2026, 9, 7, 2, 0);
        let off_peak = utc(2026, 9, 7, 12, 0);

        for model in ["deepseek-flash", "deepseek-v4-flash"] {
            approx_eq(deepseek_total_at(model, peak), 1.506);
            approx_eq(deepseek_total_at(model, off_peak), 0.753);
        }
    }

    #[test]
    fn unknown_usage_instant_keeps_peak_rates() {
        // Without a timestamp there is no time-of-day dimension to apply, so
        // pricing must stay at the published peak rates.
        approx_eq(
            calculate_total_cost_for_service_tier(
                "deepseek-v4-pro",
                ServiceTier::Standard,
                1_000_000,
                1_000_000,
                0,
                1_000_000,
            ),
            5.324,
        );
    }

    #[test]
    fn models_without_a_schedule_ignore_the_usage_instant() {
        let peak = utc(2026, 9, 7, 2, 0);
        let off_peak = utc(2026, 9, 7, 12, 0);

        for instant in [peak, off_peak] {
            approx_eq(
                calculate_total_cost_for_service_tier_at(
                    "glm-5.1",
                    ServiceTier::Standard,
                    1_000_000,
                    1_000_000,
                    0,
                    0,
                    Some(instant),
                ),
                5.80,
            );
        }
    }

    #[test]
    fn off_peak_multiplier_carries_over_to_service_tier_overrides() {
        let _guard = registry_test_guard();
        reset_global_registry();

        let mut models = HashMap::new();
        models.insert(
            "review-peak-tiered".to_string(),
            ModelInfo {
                pricing: PricingStructure::Flat {
                    input_per_1m: 10.0,
                    output_per_1m: 20.0,
                },
                caching: CachingSupport::None,
                service_tiers: HashMap::from([(
                    ServiceTier::Priority,
                    ServiceTierPricing {
                        pricing: PricingStructure::Flat {
                            input_per_1m: 20.0,
                            output_per_1m: 40.0,
                        },
                        caching: CachingSupport::None,
                    },
                )]),
                dated_pricing: Vec::new(),
                time_of_day_pricing: Some(TimeOfDayPricing {
                    timezone: "UTC".to_string(),
                    peak_windows: vec![PeakWindow::weekdays(clock(1, 0), clock(4, 0))],
                    off_peak_multiplier: 0.5,
                }),
                input_token_semantics: InputTokenSemantics::default(),
                is_estimated: false,
            },
        );
        init_external_models(models, HashMap::new());

        let peak = utc(2026, 9, 7, 2, 0);
        let off_peak = utc(2026, 9, 7, 12, 0);

        // Priority keeps its premium rates, but the time-of-day discount still
        // applies because it is a property of the clock, not of the rate card.
        approx_eq(
            calculate_input_cost_for_service_tier_at(
                "review-peak-tiered",
                ServiceTier::Priority,
                1_000_000,
                Some(peak),
            ),
            20.0,
        );
        approx_eq(
            calculate_input_cost_for_service_tier_at(
                "review-peak-tiered",
                ServiceTier::Priority,
                1_000_000,
                Some(off_peak),
            ),
            10.0,
        );
        approx_eq(
            calculate_input_cost_for_service_tier_at(
                "review-peak-tiered",
                ServiceTier::Standard,
                1_000_000,
                Some(off_peak),
            ),
            5.0,
        );
    }

    #[test]
    fn wrapping_peak_windows_use_the_configured_timezone() {
        let _guard = registry_test_guard();
        reset_global_registry();

        // A 23:00-09:00 window in Beijing time: 23:00 CST is 15:00 UTC, and
        // 08:30 CST the next day is 00:30 UTC, so the window straddles the UTC
        // date boundary as well as local midnight.
        let mut models = HashMap::new();
        models.insert(
            "review-wrapping-window".to_string(),
            ModelInfo {
                pricing: PricingStructure::Flat {
                    input_per_1m: 4.0,
                    output_per_1m: 4.0,
                },
                caching: CachingSupport::None,
                service_tiers: HashMap::new(),
                dated_pricing: Vec::new(),
                time_of_day_pricing: Some(TimeOfDayPricing {
                    timezone: "Asia/Shanghai".to_string(),
                    peak_windows: vec![PeakWindow::weekdays(clock(23, 0), clock(9, 0))],
                    off_peak_multiplier: 0.5,
                }),
                input_token_semantics: InputTokenSemantics::default(),
                is_estimated: false,
            },
        );
        init_external_models(models, HashMap::new());

        let cost_at = |instant| {
            calculate_input_cost_for_service_tier_at(
                "review-wrapping-window",
                ServiceTier::Standard,
                1_000_000,
                Some(instant),
            )
        };

        // Monday 23:30 CST, the window's own day.
        approx_eq(cost_at(utc(2026, 9, 7, 15, 30)), 4.0);
        // Tuesday 08:30 CST: still inside the window that started Monday.
        approx_eq(cost_at(utc(2026, 9, 8, 0, 30)), 4.0);
        // Tuesday 09:00 CST: the exclusive end of the Monday window.
        approx_eq(cost_at(utc(2026, 9, 8, 1, 0)), 2.0);
        // Saturday 23:30 CST: the weekday filter excludes the weekend.
        approx_eq(cost_at(utc(2026, 9, 12, 15, 30)), 2.0);
    }

    #[test]
    fn invalid_time_of_day_schedules_are_skipped() {
        let _guard = registry_test_guard();
        reset_global_registry();

        let schedule = |schedule: TimeOfDayPricing| ModelInfo {
            pricing: PricingStructure::Flat {
                input_per_1m: 1.0,
                output_per_1m: 1.0,
            },
            caching: CachingSupport::None,
            service_tiers: HashMap::new(),
            dated_pricing: Vec::new(),
            time_of_day_pricing: Some(schedule),
            input_token_semantics: InputTokenSemantics::default(),
            is_estimated: false,
        };

        let mut models = HashMap::new();
        models.insert(
            "review-zero-multiplier".to_string(),
            schedule(TimeOfDayPricing {
                timezone: "UTC".to_string(),
                peak_windows: Vec::new(),
                off_peak_multiplier: 0.0,
            }),
        );
        models.insert(
            "review-empty-window".to_string(),
            schedule(TimeOfDayPricing {
                timezone: "UTC".to_string(),
                peak_windows: vec![PeakWindow::weekdays(clock(9, 0), clock(9, 0))],
                off_peak_multiplier: 0.5,
            }),
        );
        models.insert(
            "review-out-of-range-window".to_string(),
            schedule(TimeOfDayPricing {
                timezone: "UTC".to_string(),
                peak_windows: vec![PeakWindow::weekdays(clock(9, 0), 24 * 60 + 1)],
                off_peak_multiplier: 0.5,
            }),
        );
        init_external_models(models, HashMap::new());

        for model in [
            "review-zero-multiplier",
            "review-empty-window",
            "review-out-of-range-window",
        ] {
            assert!(
                get_model_info(model).is_none(),
                "`{model}` should be rejected"
            );
        }
    }

    fn flat_rate(input: f64) -> PricingStructure {
        PricingStructure::Flat {
            input_per_1m: input,
            output_per_1m: input,
        }
    }

    fn period_ending(
        until: NaiveDate,
        input: f64,
        schedule: Option<TimeOfDayPricing>,
    ) -> DatedPricing {
        DatedPricing {
            valid_until: until,
            pricing: flat_rate(input),
            caching: CachingSupport::None,
            service_tiers: HashMap::new(),
            time_of_day_pricing: schedule,
        }
    }

    fn model_with(
        base_input: f64,
        dated_pricing: Vec<DatedPricing>,
        schedule: Option<TimeOfDayPricing>,
    ) -> ModelInfo {
        ModelInfo {
            pricing: flat_rate(base_input),
            caching: CachingSupport::None,
            service_tiers: HashMap::new(),
            dated_pricing,
            time_of_day_pricing: schedule,
            input_token_semantics: InputTokenSemantics::default(),
            is_estimated: false,
        }
    }

    fn input_cost_at(model: &str, y: i32, m: u32, d: u32, hour: u32) -> f64 {
        calculate_input_cost_for_service_tier_at(
            model,
            ServiceTier::Standard,
            1_000_000,
            Some(utc(y, m, d, hour, 0)),
        )
    }

    /// Several configured periods must each take effect, regardless of the order
    /// they were supplied in, and `valid_until` must behave as an exclusive
    /// upper bound (the boundary date belongs to the newer period).
    #[test]
    fn each_configured_period_takes_effect() {
        let _guard = registry_test_guard();
        reset_global_registry();

        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
        let mut models = HashMap::new();
        models.insert(
            "review-periods".to_string(),
            model_with(
                40.0,
                vec![
                    period_ending(day(2026, 12, 1), 30.0, None),
                    period_ending(day(2026, 6, 1), 10.0, None),
                    period_ending(day(2026, 9, 1), 20.0, None),
                ],
                None,
            ),
        );
        init_external_models(models, HashMap::new());

        for (y, m, d, expected) in [
            (2020, 1, 1, 10.0),
            (2026, 5, 31, 10.0),
            (2026, 6, 1, 20.0),
            (2026, 8, 31, 20.0),
            (2026, 9, 1, 30.0),
            (2026, 11, 30, 30.0),
            (2026, 12, 1, 40.0),
            (2030, 1, 1, 40.0),
        ] {
            approx_eq(input_cost_at("review-periods", y, m, d, 12), expected);
        }
    }

    /// Each period may run its own peak/off-peak rule; a period without one
    /// falls back to the model-level schedule.
    #[test]
    fn each_period_can_carry_its_own_peak_schedule() {
        let _guard = registry_test_guard();
        reset_global_registry();

        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
        let model_schedule = TimeOfDayPricing {
            timezone: "UTC".to_string(),
            peak_windows: vec![PeakWindow::weekdays(clock(1, 0), clock(4, 0))],
            off_peak_multiplier: 0.5,
        };
        let period_schedule = TimeOfDayPricing {
            timezone: "UTC".to_string(),
            peak_windows: vec![PeakWindow::weekdays(clock(12, 0), clock(13, 0))],
            off_peak_multiplier: 0.25,
        };

        let mut models = HashMap::new();
        models.insert(
            "review-per-period-schedule".to_string(),
            model_with(
                100.0,
                vec![
                    period_ending(day(2026, 9, 1), 100.0, None),
                    period_ending(day(2026, 12, 1), 100.0, Some(period_schedule)),
                ],
                Some(model_schedule),
            ),
        );
        init_external_models(models, HashMap::new());

        // First period has no schedule of its own, so the model-level one applies.
        approx_eq(
            input_cost_at("review-per-period-schedule", 2026, 6, 15, 2),
            100.0,
        );
        approx_eq(
            input_cost_at("review-per-period-schedule", 2026, 6, 15, 12),
            50.0,
        );

        // Second period replaces it outright.
        approx_eq(
            input_cost_at("review-per-period-schedule", 2026, 11, 16, 12),
            100.0,
        );
        approx_eq(
            input_cost_at("review-per-period-schedule", 2026, 11, 16, 14),
            25.0,
        );

        // Past every period the base rates use the model-level schedule again.
        approx_eq(
            input_cost_at("review-per-period-schedule", 2026, 12, 15, 12),
            50.0,
        );
    }

    /// Two periods ending on the same date are rejected rather than resolved by
    /// vector order.
    #[test]
    fn duplicate_period_end_dates_are_rejected() {
        let _guard = registry_test_guard();
        reset_global_registry();

        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
        let mut models = HashMap::new();
        models.insert(
            "review-duplicate-periods".to_string(),
            model_with(
                90.0,
                vec![
                    period_ending(day(2026, 9, 1), 20.0, None),
                    period_ending(day(2026, 9, 1), 25.0, None),
                ],
                None,
            ),
        );
        init_external_models(models, HashMap::new());

        assert!(
            get_model_info("review-duplicate-periods").is_none(),
            "a model with two periods ending on the same date must be rejected"
        );
    }

    #[test]
    #[should_panic(expected = "each period must end on a distinct date")]
    fn push_dated_pricing_rejects_a_duplicate_end_date() {
        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
        let mut info = model_with(90.0, Vec::new(), None);
        push_dated_pricing(&mut info, "m", period_ending(day(2026, 9, 1), 20.0, None));
        push_dated_pricing(&mut info, "m", period_ending(day(2026, 9, 1), 25.0, None));
    }

    /// A period-scoped override pointing at a date that has no period is a hard
    /// error, not a silent no-op.
    #[test]
    #[should_panic(expected = "found no period ending")]
    fn dated_period_mut_panics_for_an_unknown_end_date() {
        let day = |y, m, d| NaiveDate::from_ymd_opt(y, m, d).expect("valid date");
        let mut info = model_with(90.0, vec![period_ending(day(2026, 9, 1), 20.0, None)], None);
        dated_period_mut(&mut info, "m", day(2026, 10, 1), "add_time_of_day_pricing");
    }
}
