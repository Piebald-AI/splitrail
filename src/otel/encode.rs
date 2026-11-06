//! OTLP/HTTP JSON encoding.
//!
//! Hand-rolled rather than SDK-backed. OTLP JSON maps 1:1 onto the protobuf
//! messages and is small enough to keep in-tree, which avoids pulling the
//! OpenTelemetry dependency tree into a binary that is mostly a TUI.
//!
//! Wire format notes that matter for compatibility:
//! - `int64`/`uint64` fields are JSON *strings* (protobuf JSON mapping).
//! - Timestamps are nanoseconds since the Unix epoch.
//! - `AnyValue` is externally tagged, e.g. `{"stringValue": "chat"}`.

use chrono::{DateTime, Utc};
use serde::Serialize;

/// `SpanKind::SPAN_KIND_CLIENT`.
pub const SPAN_KIND_CLIENT: i32 = 3;

const SDK_NAME: &str = "splitrail";

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportTraceRequest {
    pub resource_spans: Vec<ResourceSpans>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceSpans {
    pub resource: Resource,
    pub scope_spans: Vec<ScopeSpans>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    pub attributes: Vec<KeyValue>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScopeSpans {
    pub scope: InstrumentationScope,
    pub spans: Vec<Span>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InstrumentationScope {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub trace_id: String,
    pub span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
    pub name: String,
    pub kind: i32,
    pub start_time_unix_nano: String,
    pub end_time_unix_nano: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<KeyValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<SpanStatus>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<SpanEvent>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpanStatus {
    /// `1` = OK, `2` = ERROR. Unset status is omitted entirely.
    pub code: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpanEvent {
    pub name: String,
    pub time_unix_nano: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<KeyValue>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct KeyValue {
    pub key: String,
    pub value: AnyValue,
}

// The `*Value` postfix is the wire format (`stringValue`, `intValue`, ...),
// so the variant names have to match it.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum AnyValue {
    StringValue(String),
    /// int64 is a string on the wire; see the module docs.
    IntValue(String),
    DoubleValue(f64),
}

pub fn attr_str(key: impl Into<String>, value: impl Into<String>) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: AnyValue::StringValue(value.into()),
    }
}

pub fn attr_int(key: impl Into<String>, value: u64) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: AnyValue::IntValue(value.to_string()),
    }
}

pub fn attr_double(key: impl Into<String>, value: f64) -> KeyValue {
    KeyValue {
        key: key.into(),
        value: AnyValue::DoubleValue(value),
    }
}

/// Non-negative nanoseconds since the Unix epoch, or `None` outside the
/// timestamp range supported by this exporter.
pub fn nanos(timestamp: &DateTime<Utc>) -> Option<String> {
    timestamp
        .timestamp_nanos_opt()
        .filter(|nanos| *nanos >= 0)
        .map(|nanos| nanos.to_string())
}

/// Resource attributes shared by every span in a run.
pub fn resource_for(service_name: &str) -> Resource {
    let mut attributes = vec![
        attr_str("service.name", service_name),
        attr_str("service.version", env!("CARGO_PKG_VERSION")),
        attr_str("telemetry.sdk.name", SDK_NAME),
        attr_str("telemetry.sdk.language", "rust"),
        attr_str("telemetry.sdk.version", env!("CARGO_PKG_VERSION")),
    ];
    if let Some(host) = host_name() {
        attributes.push(attr_str("host.name", host));
    }
    Resource { attributes }
}

fn host_name() -> Option<String> {
    std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .filter(|value| !value.trim().is_empty())
}

/// Build an export request for one batch of spans.
pub fn request_from_spans(resource: &Resource, spans: Vec<Span>) -> ExportTraceRequest {
    ExportTraceRequest {
        resource_spans: vec![ResourceSpans {
            resource: resource.clone(),
            scope_spans: vec![ScopeSpans {
                scope: InstrumentationScope {
                    name: SDK_NAME.to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                spans,
            }],
        }],
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

/// `AggregationTemporality::AGGREGATION_TEMPORALITY_DELTA`.
pub const TEMPORALITY_DELTA: i32 = 1;

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ExportMetricsRequest {
    pub resource_metrics: Vec<ResourceMetrics>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ResourceMetrics {
    pub resource: Resource,
    pub scope_metrics: Vec<ScopeMetrics>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ScopeMetrics {
    pub scope: InstrumentationScope,
    pub metrics: Vec<Metric>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
    pub name: String,
    pub description: String,
    pub unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sum: Option<Sum>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub histogram: Option<Histogram>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Sum {
    pub data_points: Vec<NumberDataPoint>,
    pub aggregation_temporality: i32,
    pub is_monotonic: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Histogram {
    pub data_points: Vec<HistogramDataPoint>,
    pub aggregation_temporality: i32,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct NumberDataPoint {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<KeyValue>,
    pub start_time_unix_nano: String,
    pub time_unix_nano: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_double: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub as_int: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistogramDataPoint {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub attributes: Vec<KeyValue>,
    pub start_time_unix_nano: String,
    pub time_unix_nano: String,
    pub count: String,
    pub sum: f64,
    pub bucket_counts: Vec<String>,
    pub explicit_bounds: Vec<f64>,
}

/// Build a metrics request from already-grouped metrics.
pub fn metrics_request(resource: &Resource, metrics: Vec<Metric>) -> ExportMetricsRequest {
    ExportMetricsRequest {
        resource_metrics: vec![ResourceMetrics {
            resource: resource.clone(),
            scope_metrics: vec![ScopeMetrics {
                scope: InstrumentationScope {
                    name: SDK_NAME.to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                metrics,
            }],
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn timestamps_are_nanos_as_strings() {
        let at = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(nanos(&at).as_deref(), Some("1767323045000000000"));
    }

    #[test]
    fn timestamp_range_preserves_epoch_and_rejects_invalid_dates() {
        assert_eq!(nanos(&DateTime::UNIX_EPOCH).as_deref(), Some("0"));
        for at in [
            DateTime::UNIX_EPOCH - chrono::Duration::nanoseconds(1),
            DateTime::<Utc>::MAX_UTC,
            DateTime::<Utc>::MIN_UTC,
        ] {
            assert_eq!(nanos(&at), None);
        }
    }

    #[test]
    fn int_values_serialize_as_strings() {
        let encoded = simd_json::to_string(&attr_int("gen_ai.usage.output_tokens", 42)).unwrap();
        assert_eq!(
            encoded,
            r#"{"key":"gen_ai.usage.output_tokens","value":{"intValue":"42"}}"#
        );
    }

    #[test]
    fn doubles_serialize_as_numbers() {
        assert_eq!(
            simd_json::to_string(&attr_double("splitrail.cost.usd", 0.5)).unwrap(),
            r#"{"key":"splitrail.cost.usd","value":{"doubleValue":0.5}}"#
        );
    }

    #[test]
    fn resource_carries_service_and_sdk_identity() {
        let resource = resource_for("splitrail");
        let keys: Vec<&str> = resource
            .attributes
            .iter()
            .map(|attribute| attribute.key.as_str())
            .collect();
        assert!(keys.contains(&"service.name"));
        assert!(keys.contains(&"telemetry.sdk.language"));
        assert!(keys.contains(&"telemetry.sdk.version"));
    }

    #[test]
    fn empty_parent_and_attrs_are_omitted() {
        let span = Span {
            trace_id: "a".repeat(32),
            span_id: "b".repeat(16),
            parent_span_id: None,
            name: "chat gpt-5".to_string(),
            kind: SPAN_KIND_CLIENT,
            start_time_unix_nano: "1".to_string(),
            end_time_unix_nano: "2".to_string(),
            attributes: Vec::new(),
            status: None,
            events: Vec::new(),
        };
        let json = simd_json::to_string(&span).unwrap();
        assert!(!json.contains("parentSpanId"));
        assert!(!json.contains("attributes"));
        assert!(!json.contains("status"));
        assert!(json.contains("\"kind\":3"));
    }
}
