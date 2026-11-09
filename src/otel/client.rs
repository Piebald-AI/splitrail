//! OTLP/HTTP transport.
//!
//! Uses `reqwest` directly, so the exporter adds no TLS stack, no connection
//! pool, and no dependency beyond what the upload path already links.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use simd_json::prelude::*;

use super::config::OtlpTarget;
use super::encode::{ExportMetricsRequest, ExportTraceRequest};

/// HTTP statuses worth retrying: rate limits, timeouts, collector restarts.
fn is_retryable(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

pub struct OtlpClient {
    client: reqwest::Client,
    traces_endpoint: String,
    metrics_endpoint: Option<String>,
    headers: Vec<(String, String)>,
    max_retries: u32,
}

impl OtlpClient {
    pub fn new(traces: &OtlpTarget, metrics: Option<&OtlpTarget>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(traces.timeout_secs))
            .build()
            .context("Failed to create OTLP HTTP client")?;
        Ok(Self {
            client,
            traces_endpoint: traces.endpoint.clone(),
            metrics_endpoint: metrics.map(|target| target.endpoint.clone()),
            headers: traces
                .headers
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
            max_retries: traces.max_retries,
        })
    }

    pub async fn post_traces(&self, request: &ExportTraceRequest) -> Result<()> {
        let body = simd_json::to_vec(request).context("Failed to serialize OTLP traces")?;
        self.post(&self.traces_endpoint, "traces", body).await
    }

    pub async fn post_metrics(&self, request: &ExportMetricsRequest) -> Result<()> {
        let Some(endpoint) = self.metrics_endpoint.clone() else {
            bail!("No OTLP metrics endpoint resolved");
        };
        let body = simd_json::to_vec(request).context("Failed to serialize OTLP metrics")?;
        self.post(&endpoint, "metrics", body).await
    }

    async fn post(&self, endpoint: &str, signal: &str, body: Vec<u8>) -> Result<()> {
        let attempts = self.max_retries.max(1);
        let mut last_error = None;

        for attempt in 0..attempts {
            if attempt > 0 {
                let backoff = Duration::from_millis(250 * 2u64.pow(attempt - 1));
                tokio::time::sleep(backoff).await;
            }

            let mut request = self
                .client
                .post(endpoint)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
            for (key, value) in &self.headers {
                request = request.header(key.as_str(), value.as_str());
            }

            match request.send().await {
                Ok(response) => {
                    let status = response.status();
                    if status.is_success() {
                        check_partial_success(response, signal).await;
                        return Ok(());
                    }
                    let text = response.text().await.unwrap_or_default();
                    if !is_retryable(status) {
                        bail!("OTLP {signal} request to {endpoint} failed: {status} {text}");
                    }
                    last_error = Some(anyhow::anyhow!(
                        "OTLP {signal} request to {endpoint} failed: {status} {text}"
                    ));
                }
                Err(error) => {
                    last_error = Some(anyhow::anyhow!(
                        "OTLP {signal} request to {endpoint} failed: {error}"
                    ));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::anyhow!("OTLP {signal} export failed")))
    }
}

/// A collector can accept a request and still reject part of it. Report that
/// instead of claiming success on data the collector dropped.
async fn check_partial_success(response: reqwest::Response, signal: &str) {
    let Ok(body) = response.text().await else {
        return;
    };
    if body.trim().is_empty() {
        return;
    }
    let mut bytes = body.into_bytes();
    let Ok(parsed) = simd_json::from_slice::<simd_json::OwnedValue>(&mut bytes) else {
        return;
    };
    let Some(partial) = parsed.get("partialSuccess") else {
        return;
    };
    let rejected = rejected_count(partial, "rejectedSpans")
        .or_else(|| rejected_count(partial, "rejectedDataPoints"))
        .unwrap_or(0);
    if rejected > 0 {
        let message = partial
            .get("errorMessage")
            .and_then(|value| value.as_str())
            .unwrap_or("no message");
        eprintln!("⚠️  Collector rejected {rejected} {signal} item(s): {message}");
    }
}

fn rejected_count(partial: &simd_json::OwnedValue, field: &str) -> Option<i64> {
    let value = partial.get(field)?;
    value
        .as_i64()
        .or_else(|| value.as_str()?.parse::<i64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(endpoint: &str) -> OtlpTarget {
        OtlpTarget {
            endpoint: endpoint.to_string(),
            headers: Default::default(),
            service_name: "splitrail".to_string(),
            timeout_secs: 5,
            max_retries: 2,
        }
    }

    #[test]
    fn client_builds_from_target() {
        let traces = target("http://collector:4318/v1/traces");
        let client = OtlpClient::new(&traces, None).unwrap();
        assert_eq!(client.traces_endpoint, "http://collector:4318/v1/traces");
        assert!(client.metrics_endpoint.is_none());
        assert_eq!(client.max_retries, 2);
    }

    #[test]
    fn retryable_statuses() {
        assert!(is_retryable(reqwest::StatusCode::TOO_MANY_REQUESTS));
        assert!(is_retryable(reqwest::StatusCode::BAD_GATEWAY));
        assert!(!is_retryable(reqwest::StatusCode::BAD_REQUEST));
    }

    #[test]
    fn empty_payload_serializes() {
        let request = ExportTraceRequest {
            resource_spans: Vec::new(),
        };
        assert!(simd_json::to_vec(&request).is_ok());
    }

    #[test]
    fn partial_success_rejection_accepts_json_numbers_and_int64_strings() {
        for body in [
            r#"{"partialSuccess":{"rejectedSpans":3}}"#,
            r#"{"partialSuccess":{"rejectedSpans":"3"}}"#,
            r#"{"partialSuccess":{"rejectedDataPoints":"4"}}"#,
        ] {
            let mut bytes = body.as_bytes().to_vec();
            let parsed = simd_json::from_slice::<simd_json::OwnedValue>(&mut bytes).unwrap();
            let partial = parsed.get("partialSuccess").unwrap();
            let rejected = rejected_count(partial, "rejectedSpans")
                .or_else(|| rejected_count(partial, "rejectedDataPoints"));
            assert!(rejected.unwrap() > 0);
        }
    }
}
