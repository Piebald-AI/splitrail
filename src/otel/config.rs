//! OTel exporter configuration and OTLP target resolution.
//!
//! Precedence for every value is command line > environment > `[otel]` section
//! in `~/.splitrail.toml` > built-in default, so an administrator can point a
//! whole fleet at a collector with `OTEL_EXPORTER_OTLP_ENDPOINT` alone.

use std::collections::HashMap;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

fn default_endpoint() -> String {
    "http://localhost:4318".to_string()
}

fn default_protocol() -> String {
    "http_json".to_string()
}

fn default_service_name() -> String {
    "splitrail".to_string()
}

fn default_batch_size() -> usize {
    512
}

fn default_interval_secs() -> u64 {
    15
}

fn default_timeout_secs() -> u64 {
    30
}

fn default_max_retries() -> u32 {
    3
}

/// `[otel]` section of the configuration file.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct OtelConfig {
    /// Whether the TUI should run the exporter in the background. The
    /// subcommands work regardless; this only controls automatic start.
    pub enabled: bool,
    /// Base OTLP endpoint. `/v1/traces` and `/v1/metrics` are appended, unless
    /// the value already names a signal path.
    pub endpoint: String,
    /// Transport. Only OTLP/HTTP JSON is implemented; anything else is
    /// rejected explicitly.
    pub protocol: String,
    /// Extra headers sent with every request (collector auth, tenant ids).
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// `service.name` resource attribute.
    pub service_name: String,
    /// Emit `execute_tool` spans. Requires analyzer support.
    #[serde(default)]
    pub include_tool_spans: bool,
    /// Analyzers that already instrument themselves and should be skipped.
    #[serde(default)]
    pub exclude_analyzers: Vec<String>,
    /// How much history a first export covers. `0` means everything.
    #[serde(default)]
    pub backfill_days: i64,
    /// Maximum spans per OTLP request.
    #[serde(default = "default_batch_size")]
    pub max_batch_size: usize,
    /// Live export interval.
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// How long a message must sit unchanged before live export sends it.
    /// `0` uses the built-in default (10s).
    #[serde(default)]
    pub settle_secs: u64,
    /// Per-request timeout.
    #[serde(default = "default_timeout_secs")]
    pub timeout_secs: u64,
    /// Attempts per request before the run fails.
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: default_endpoint(),
            protocol: default_protocol(),
            headers: HashMap::new(),
            service_name: default_service_name(),
            include_tool_spans: false,
            exclude_analyzers: Vec::new(),
            backfill_days: 0,
            max_batch_size: default_batch_size(),
            interval_secs: default_interval_secs(),
            settle_secs: 0,
            timeout_secs: default_timeout_secs(),
            max_retries: default_max_retries(),
        }
    }
}

/// A resolved OTLP destination for one signal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpTarget {
    pub endpoint: String,
    pub headers: HashMap<String, String>,
    pub service_name: String,
    pub timeout_secs: u64,
    pub max_retries: u32,
}

impl OtlpTarget {
    /// Fail early with an actionable message instead of at the first POST.
    pub fn validate(&self) -> Result<()> {
        if self.endpoint.trim().is_empty() {
            bail!(
                "No OTLP endpoint configured. Set `otel.endpoint` in the config file, \
                 pass `--endpoint`, or export OTEL_EXPORTER_OTLP_ENDPOINT."
            );
        }
        Ok(())
    }
}

impl OtelConfig {
    /// Is this analyzer opted out of export?
    pub fn excludes(&self, analyzer_name: &str) -> bool {
        self.exclude_analyzers
            .iter()
            .any(|excluded| excluded.eq_ignore_ascii_case(analyzer_name))
    }

    /// Resolve the traces destination.
    pub fn resolve_traces(&self, endpoint_override: Option<&str>) -> OtlpTarget {
        self.resolve_signal("traces", endpoint_override)
    }

    /// Resolve the metrics destination.
    pub fn resolve_metrics(&self, endpoint_override: Option<&str>) -> OtlpTarget {
        self.resolve_signal("metrics", endpoint_override)
    }

    fn resolve_signal(&self, signal: &str, endpoint_override: Option<&str>) -> OtlpTarget {
        let signal_env = format!("OTEL_EXPORTER_OTLP_{}_ENDPOINT", signal.to_uppercase());
        let endpoint = endpoint_override
            .map(str::to_string)
            .or_else(|| env_non_empty(&signal_env))
            .or_else(|| env_non_empty("OTEL_EXPORTER_OTLP_ENDPOINT"))
            .unwrap_or_else(|| self.endpoint.clone());

        let mut headers: HashMap<String, String> = self.headers.clone();
        for (key, value) in parse_env_headers() {
            headers.insert(key, value);
        }

        OtlpTarget {
            endpoint: signal_url(&endpoint, signal),
            headers,
            service_name: env_non_empty("OTEL_SERVICE_NAME")
                .unwrap_or_else(|| self.service_name.clone()),
            timeout_secs: self.timeout_secs.max(1),
            max_retries: self.max_retries,
        }
    }
}

/// Append the signal path unless the endpoint already names one.
///
/// A base endpoint (`http://collector:4318`) gets `/v1/traces` appended. An
/// endpoint that names the other signal (`.../v1/metrics`) has that segment
/// replaced rather than nested, which is what happens when a user sets only
/// `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT` and still exports traces.
pub fn signal_url(endpoint: &str, signal: &str) -> String {
    let base = endpoint.trim().trim_end_matches('/');
    if signal_path_matches(base, signal) {
        return base.to_string();
    }
    match base.rsplit_once("/v1/") {
        Some((root, _)) => format!("{root}/v1/{signal}"),
        None => format!("{base}/v1/{signal}"),
    }
}

fn signal_path_matches(endpoint: &str, signal: &str) -> bool {
    match signal {
        "traces" => endpoint.ends_with("/v1/traces") || endpoint.ends_with("/v1/trace"),
        "metrics" => endpoint.ends_with("/v1/metrics") || endpoint.ends_with("/v1/metric"),
        _ => endpoint.ends_with(&format!("/v1/{signal}")),
    }
}

/// Headers from `OTEL_EXPORTER_OTLP_HEADERS`, in the spec's `k=v,k2=v2` form.
fn parse_env_headers() -> Vec<(String, String)> {
    let Some(raw) = env_non_empty("OTEL_EXPORTER_OTLP_HEADERS") else {
        return Vec::new();
    };
    raw.split(',')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            Some((key.to_string(), value.trim().to_string()))
        })
        .collect()
}

fn env_non_empty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Validate the configured protocol, so a gRPC endpoint fails loudly instead of
/// silently receiving JSON.
pub fn validate_protocol(protocol: &str) -> Result<()> {
    match protocol.trim().to_ascii_lowercase().as_str() {
        "http_json" | "http/json" | "http_protobuf" | "http/protobuf" => Ok(()),
        other => bail!(
            "Unsupported OTLP protocol {other:?}. Only OTLP/HTTP JSON is implemented; \
             leave `otel.protocol` at its default."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_url_appends_to_base_endpoint() {
        assert_eq!(
            signal_url("http://localhost:4318", "traces"),
            "http://localhost:4318/v1/traces"
        );
        assert_eq!(
            signal_url("http://localhost:4318/", "metrics"),
            "http://localhost:4318/v1/metrics"
        );
    }

    #[test]
    fn signal_url_keeps_matching_signal_path() {
        assert_eq!(
            signal_url("http://collector:4318/v1/traces", "traces"),
            "http://collector:4318/v1/traces"
        );
    }

    #[test]
    fn signal_url_replaces_other_signal_path() {
        assert_eq!(
            signal_url("http://collector:4318/v1/metrics", "traces"),
            "http://collector:4318/v1/traces"
        );
    }

    #[test]
    fn excludes_is_case_insensitive() {
        let config = OtelConfig {
            exclude_analyzers: vec!["Claude Code".to_string()],
            ..Default::default()
        };
        assert!(config.excludes("claude code"));
        assert!(!config.excludes("Codex CLI"));
    }

    #[test]
    fn validate_protocol_rejects_grpc() {
        assert!(validate_protocol("grpc").is_err());
        assert!(validate_protocol("http_json").is_ok());
    }

    #[test]
    fn default_config_is_disabled_and_opt_out_free() {
        let config = OtelConfig::default();
        assert!(!config.enabled);
        assert!(config.exclude_analyzers.is_empty());
        assert_eq!(config.backfill_days, 0);
    }
}
