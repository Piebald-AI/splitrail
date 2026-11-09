//! OTLP metrics built from the same messages the spans come from.
//!
//! Token counts are the part of this export that is measured rather than
//! inferred (the model reported them), so they are what belongs in a metrics
//! backend. Request duration is *not* exported as a metric: transcripts do not
//! record it, and a derived number in a histogram invites SLOs on fiction. Turn
//! and request spans carry derived timings instead, tagged
//! `splitrail.timing = "derived"`.
//!
//! Data points use exporter-owned, non-overlapping delta intervals. Message
//! timestamps do not partition series, so backfills and same-time messages are
//! aggregated into the next export interval.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use crate::types::{ConversationMessage, MessageRole, Stats};

use super::encode::{
    self, Histogram, HistogramDataPoint, KeyValue, Metric, NumberDataPoint, Sum, TEMPORALITY_DELTA,
    attr_str,
};

/// Histogram of tokens per request, split by token type.
pub const TOKEN_USAGE_METRIC: &str = "gen_ai.client.token.usage";

/// Sum counter of cost in US dollars.
pub const COST_METRIC: &str = "splitrail.cost.usd";

/// Bucket boundaries for tokens per request. Wide enough to cover a cached
/// million-token prompt without collapsing every large request into one
/// overflow bucket.
const TOKEN_BUCKETS: [f64; 16] = [
    1.0,
    4.0,
    16.0,
    64.0,
    256.0,
    1024.0,
    4096.0,
    16384.0,
    65536.0,
    262_144.0,
    1_048_576.0,
    2_097_152.0,
    4_194_304.0,
    8_388_608.0,
    16_777_216.0,
    33_554_432.0,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum TokenType {
    Input,
    Output,
    CacheRead,
    CacheWrite,
}

impl TokenType {
    const ALL: [TokenType; 4] = [
        TokenType::Input,
        TokenType::Output,
        TokenType::CacheRead,
        TokenType::CacheWrite,
    ];

    fn as_str(self) -> &'static str {
        match self {
            TokenType::Input => "input",
            TokenType::Output => "output",
            TokenType::CacheRead => "cache_read",
            TokenType::CacheWrite => "cache_write",
        }
    }

    fn value(self, stats: &Stats) -> u64 {
        match self {
            TokenType::Input => stats.input_tokens,
            TokenType::Output => stats.output_tokens,
            TokenType::CacheRead => {
                if stats.cache_read_tokens == 0 {
                    stats
                        .cached_tokens
                        .saturating_sub(stats.cache_creation_tokens)
                } else {
                    stats.cache_read_tokens
                }
            }
            TokenType::CacheWrite => stats.cache_creation_tokens,
        }
    }
}

/// Dimensions a dashboard slices on; timestamps are supplied per export.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SeriesKey {
    analyzer: String,
    model: String,
    token_type: Option<TokenType>,
}

#[derive(Debug, Clone)]
struct HistogramAccumulator {
    bucket_counts: Vec<u64>,
    count: u64,
    sum: f64,
}

impl HistogramAccumulator {
    fn new() -> Self {
        Self {
            bucket_counts: vec![0; TOKEN_BUCKETS.len() + 1],
            count: 0,
            sum: 0.0,
        }
    }

    fn observe(&mut self, value: u64) {
        let value = value as f64;
        let index = TOKEN_BUCKETS
            .iter()
            .position(|bound| value <= *bound)
            .unwrap_or(TOKEN_BUCKETS.len());
        self.bucket_counts[index] += 1;
        self.count += 1;
        self.sum += value;
    }
}

/// Build delta token-usage and cost metrics for one export interval.
pub fn build_metrics(
    analyzer_name: &str,
    messages: &[ConversationMessage],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<Metric> {
    let (Some(start_nanos), Some(end_nanos)) = (encode::nanos(&start), encode::nanos(&end)) else {
        return Vec::new();
    };
    let mut histograms: BTreeMap<SeriesKey, HistogramAccumulator> = BTreeMap::new();
    let mut costs: BTreeMap<SeriesKey, f64> = BTreeMap::new();

    for message in messages {
        if message.role != MessageRole::Assistant
            || message
                .date
                .timestamp_nanos_opt()
                .is_none_or(|nanos| nanos < 0)
        {
            continue;
        }
        let model = message
            .model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or("unknown")
            .to_string();
        for token_type in TokenType::ALL {
            let value = token_type.value(&message.stats);
            if value == 0 {
                continue;
            }
            histograms
                .entry(SeriesKey {
                    analyzer: analyzer_name.to_string(),
                    model: model.clone(),
                    token_type: Some(token_type),
                })
                .or_insert_with(HistogramAccumulator::new)
                .observe(value);
        }
        if message.stats.cost != 0.0 {
            *costs
                .entry(SeriesKey {
                    analyzer: analyzer_name.to_string(),
                    model,
                    token_type: None,
                })
                .or_insert(0.0) += message.stats.cost;
        }
    }

    let mut metrics = Vec::new();

    if !histograms.is_empty() {
        let data_points = histograms
            .into_iter()
            .map(|(key, accumulator)| HistogramDataPoint {
                attributes: base_attributes(&key),
                start_time_unix_nano: start_nanos.clone(),
                time_unix_nano: end_nanos.clone(),
                count: accumulator.count.to_string(),
                sum: accumulator.sum,
                bucket_counts: accumulator
                    .bucket_counts
                    .into_iter()
                    .map(|count| count.to_string())
                    .collect(),
                explicit_bounds: TOKEN_BUCKETS.to_vec(),
            })
            .collect();
        metrics.push(Metric {
            name: TOKEN_USAGE_METRIC.to_string(),
            description: "Number of tokens processed per request".to_string(),
            unit: "{token}".to_string(),
            sum: None,
            histogram: Some(Histogram {
                data_points,
                aggregation_temporality: TEMPORALITY_DELTA,
            }),
        });
    }

    if !costs.is_empty() {
        let data_points = costs
            .into_iter()
            .map(|(key, cost)| NumberDataPoint {
                attributes: base_attributes(&key),
                start_time_unix_nano: start_nanos.clone(),
                time_unix_nano: end_nanos.clone(),
                as_double: Some(cost),
                as_int: None,
            })
            .collect();
        metrics.push(Metric {
            name: COST_METRIC.to_string(),
            description: "Cost of requests in US dollars".to_string(),
            unit: "USD".to_string(),
            sum: Some(Sum {
                data_points,
                aggregation_temporality: TEMPORALITY_DELTA,
                is_monotonic: true,
            }),
            histogram: None,
        });
    }

    metrics
}

fn base_attributes(key: &SeriesKey) -> Vec<KeyValue> {
    let mut attributes = vec![
        attr_str("splitrail.analyzer", key.analyzer.clone()),
        attr_str("gen_ai.request.model", key.model.clone()),
    ];
    if let Some(token_type) = key.token_type {
        attributes.push(attr_str("gen_ai.token.type", token_type.as_str()));
    }
    attributes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::otel::encode::AnyValue;
    use crate::types::{Application, MessageRole};
    use chrono::{Duration, TimeZone};

    fn base() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 5, 10, 15, 0).unwrap()
    }

    fn message(offset_minutes: i64, stats: Stats) -> ConversationMessage {
        ConversationMessage {
            application: Application::CodexCli,
            date: base() + Duration::minutes(offset_minutes),
            project_hash: "project".to_string(),
            project_path: None,
            conversation_hash: "conv".to_string(),
            local_hash: None,
            global_hash: format!("hash-{offset_minutes}"),
            model: Some("gpt-5.6-codex".to_string()),
            stats,
            is_tool_result: false,
            role: MessageRole::Assistant,
            uuid: None,
            session_name: None,
        }
    }

    fn histogram(metrics: &[Metric]) -> Histogram {
        metrics
            .iter()
            .find(|metric| metric.name == TOKEN_USAGE_METRIC)
            .and_then(|metric| metric.histogram.clone())
            .expect("token usage histogram")
    }

    fn sum(metrics: &[Metric]) -> Sum {
        metrics
            .iter()
            .find(|metric| metric.name == COST_METRIC)
            .and_then(|metric| metric.sum.clone())
            .expect("cost sum")
    }

    fn token_types(histogram: &Histogram) -> Vec<String> {
        histogram
            .data_points
            .iter()
            .filter_map(|point| {
                point.attributes.iter().find_map(|attribute| {
                    if attribute.key == "gen_ai.token.type"
                        && let AnyValue::StringValue(value) = &attribute.value
                    {
                        Some(value.clone())
                    } else {
                        None
                    }
                })
            })
            .collect()
    }

    #[test]
    fn input_and_output_become_separate_series() {
        let messages = vec![message(
            0,
            Stats {
                input_tokens: 100,
                output_tokens: 10,
                ..Stats::default()
            },
        )];
        let metrics = build_metrics(
            "Codex CLI",
            &messages,
            base(),
            base() + Duration::seconds(1),
        );
        let histogram = histogram(&metrics);

        assert_eq!(histogram.data_points.len(), 2);
        assert_eq!(token_types(&histogram), vec!["input", "output"]);
        for point in &histogram.data_points {
            assert_eq!(point.count, "1");
            assert_eq!(point.bucket_counts.len(), TOKEN_BUCKETS.len() + 1);
        }
        let input_point = histogram
            .data_points
            .iter()
            .find(|point| point_has_token_type(point, "input"))
            .unwrap();
        assert_eq!(input_point.sum, 100.0);
        assert_eq!(histogram.aggregation_temporality, TEMPORALITY_DELTA);
    }

    fn point_has_token_type(point: &HistogramDataPoint, wanted: &str) -> bool {
        point.attributes.iter().any(|attribute| {
            attribute.key == "gen_ai.token.type"
                && matches!(&attribute.value, AnyValue::StringValue(value) if value == wanted)
        })
    }

    #[test]
    fn cache_tokens_are_distinguished() {
        let messages = vec![message(
            0,
            Stats {
                cache_read_tokens: 900,
                cache_creation_tokens: 30,
                ..Stats::default()
            },
        )];
        let metrics = build_metrics(
            "Claude Code",
            &messages,
            base(),
            base() + Duration::seconds(1),
        );
        assert_eq!(
            token_types(&histogram(&metrics)),
            vec!["cache_read", "cache_write"]
        );
    }

    #[test]
    fn cached_tokens_fall_back_to_cache_read_when_detailed_count_is_absent() {
        let metrics = build_metrics(
            "Codex CLI",
            &[message(
                0,
                Stats {
                    cached_tokens: 900,
                    ..Stats::default()
                },
            )],
            base(),
            base() + Duration::seconds(1),
        );
        let cached_histogram = histogram(&metrics);
        assert_eq!(token_types(&cached_histogram), vec!["cache_read"]);
        assert_eq!(cached_histogram.data_points[0].sum, 900.0);

        let detailed_wins = build_metrics(
            "Codex CLI",
            &[message(
                0,
                Stats {
                    cache_read_tokens: 12,
                    cached_tokens: 900,
                    ..Stats::default()
                },
            )],
            base(),
            base() + Duration::seconds(1),
        );
        assert_eq!(histogram(&detailed_wins).data_points[0].sum, 12.0);
    }

    #[test]
    fn cached_aggregate_containing_only_writes_does_not_emit_cache_reads() {
        let metrics = build_metrics(
            "Cline",
            &[message(
                0,
                Stats {
                    cached_tokens: 30,
                    cache_creation_tokens: 30,
                    ..Stats::default()
                },
            )],
            base(),
            base() + Duration::seconds(1),
        );
        let histogram = histogram(&metrics);
        assert_eq!(token_types(&histogram), vec!["cache_write"]);
        assert_eq!(histogram.data_points[0].sum, 30.0);
    }

    #[test]
    fn same_hour_incremental_updates_use_non_overlapping_export_intervals() {
        let start = base() - Duration::seconds(1);
        let end = base();
        let first = build_metrics(
            "Codex CLI",
            &[message(
                0,
                Stats {
                    input_tokens: 10,
                    ..Stats::default()
                },
            )],
            start,
            end,
        );
        let second = build_metrics(
            "Codex CLI",
            &[message(
                5,
                Stats {
                    input_tokens: 20,
                    ..Stats::default()
                },
            )],
            end,
            end + Duration::seconds(1),
        );
        let first_point = &histogram(&first).data_points[0];
        let second_point = &histogram(&second).data_points[0];
        assert_eq!(
            first_point.start_time_unix_nano,
            encode::nanos(&start).unwrap()
        );
        assert_eq!(first_point.time_unix_nano, encode::nanos(&end).unwrap());
        assert_eq!(
            second_point.start_time_unix_nano,
            encode::nanos(&end).unwrap()
        );
        assert_eq!(
            second_point.time_unix_nano,
            encode::nanos(&(end + Duration::seconds(1))).unwrap()
        );
        assert_eq!(first_point.count, "1");
        assert_eq!(second_point.count, "1");
    }
    #[test]
    fn backfill_timestamps_do_not_change_export_interval_or_duplicate_series() {
        let messages = vec![
            message(
                120,
                Stats {
                    input_tokens: 10,
                    ..Stats::default()
                },
            ),
            message(
                -120,
                Stats {
                    input_tokens: 20,
                    ..Stats::default()
                },
            ),
        ];
        let start = base() - Duration::seconds(1);
        let end = base();
        let result = build_metrics("Codex CLI", &messages, start, end);
        let histogram = histogram(&result);
        assert_eq!(histogram.data_points.len(), 1);
        assert_eq!(histogram.data_points[0].sum, 30.0);
        assert_eq!(
            histogram.data_points[0].start_time_unix_nano,
            encode::nanos(&start).unwrap()
        );
        assert_eq!(
            histogram.data_points[0].time_unix_nano,
            encode::nanos(&end).unwrap()
        );
    }

    #[test]
    fn no_usage_produces_no_metrics() {
        let messages = vec![
            message(0, Stats::default()),
            ConversationMessage {
                role: MessageRole::User,
                model: None,
                ..message(
                    0,
                    Stats {
                        input_tokens: 5,
                        ..Stats::default()
                    },
                )
            },
        ];
        assert!(
            build_metrics(
                "Codex CLI",
                &messages,
                base(),
                base() + Duration::seconds(1)
            )
            .is_empty()
        );
    }

    #[test]
    fn costs_are_aggregated_for_export_interval() {
        let messages = vec![
            message(
                0,
                Stats {
                    cost: 0.25,
                    ..Stats::default()
                },
            ),
            message(
                5,
                Stats {
                    cost: 0.75,
                    ..Stats::default()
                },
            ),
        ];
        let metrics = build_metrics(
            "Codex CLI",
            &messages,
            base(),
            base() + Duration::seconds(1),
        );
        let cost = sum(&metrics);
        assert_eq!(cost.data_points.len(), 1);
        assert_eq!(cost.data_points[0].as_double, Some(1.0));
        assert!(cost.is_monotonic);
    }

    #[test]
    fn large_values_land_in_the_overflow_bucket() {
        let messages = vec![message(
            0,
            Stats {
                input_tokens: 100_000_000,
                ..Stats::default()
            },
        )];
        let metrics = build_metrics(
            "Codex CLI",
            &messages,
            base(),
            base() + Duration::seconds(1),
        );
        let point = &histogram(&metrics).data_points[0];
        assert_eq!(
            point.bucket_counts.last().map(String::as_str),
            Some("1"),
            "value larger than the largest bound goes to the overflow bucket"
        );
    }

    #[test]
    fn missing_models_keep_billed_usage_and_cost() {
        for model in [None, Some(String::new())] {
            let mut reply = message(
                0,
                Stats {
                    input_tokens: 17,
                    cost: 0.25,
                    ..Stats::default()
                },
            );
            reply.model = model;
            let metrics =
                build_metrics("Codex CLI", &[reply], base(), base() + Duration::seconds(1));
            let tokens = histogram(&metrics);
            assert_eq!(tokens.data_points[0].sum, 17.0);
            assert_eq!(sum(&metrics).data_points[0].as_double, Some(0.25));
            assert!(
                tokens.data_points[0]
                    .attributes
                    .iter()
                    .any(|attribute| attribute.key == "gen_ai.request.model"
                        && attribute.value == AnyValue::StringValue("unknown".to_string()))
            );
        }
    }

    #[test]
    fn invalid_dates_do_not_contribute_to_metrics() {
        let mut valid = message(
            0,
            Stats {
                input_tokens: 7,
                ..Stats::default()
            },
        );
        valid.date = DateTime::UNIX_EPOCH;
        let mut invalid = message(
            1,
            Stats {
                input_tokens: 1000,
                cost: 1.0,
                ..Stats::default()
            },
        );
        for date in [
            DateTime::UNIX_EPOCH - Duration::nanoseconds(1),
            DateTime::<Utc>::MAX_UTC,
        ] {
            invalid.date = date;
            let metrics = build_metrics(
                "Codex CLI",
                &[invalid.clone(), valid.clone()],
                base(),
                base() + Duration::seconds(1),
            );
            assert_eq!(histogram(&metrics).data_points[0].sum, 7.0);
            assert!(metrics.iter().all(|metric| metric.name != COST_METRIC));
            assert!(build_metrics("Codex CLI", &[valid.clone()], date, base()).is_empty());
        }
    }
}
