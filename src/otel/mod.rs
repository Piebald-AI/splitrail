//! OpenTelemetry export.
//!
//! Builds OTLP/HTTP JSON from the messages the analyzers already produce, so
//! agents that have no native telemetry still show up in a collector. The
//! normalization work (parsing, dedupe, pricing) is already done by the time a
//! [`ConversationMessage`] exists, which is what makes this cheap: no new
//! parsing, no new pricing path, no server change.
//!
//! Three entry points share one sender:
//!
//! * [`run_export`] — one-shot backfill over everything on disk.
//! * [`serve::run_serve`] — follow transcript changes and export as they land.
//! * [`Exporter::send`] — the batch-level unit both use.
//!
//! Nothing runs unless a user asks: no background export, no state written, no
//! collector contacted when the feature is untouched.

pub mod client;
pub mod config;
pub mod encode;
pub mod ids;
pub mod metrics;
pub mod serve;
pub mod spans;
pub mod state;

use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};

use crate::analyzer::AnalyzerRegistry;
use crate::types::{ConversationMessage, ToolCall};

pub use config::OtelConfig;

/// Options for a one-shot export, as supplied on the command line.
#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    /// Only export this analyzer (by display name, e.g. `"Codex CLI"`).
    pub analyzer: Option<String>,
    /// Only export messages from the last N days. `None` uses the config value.
    pub days: Option<i64>,
    /// Build the payload and print it instead of sending it.
    pub dry_run: bool,
    /// Pretty-print `--dry-run` output.
    pub pretty: bool,
    /// Override the configured endpoint.
    pub endpoint: Option<String>,
    /// Also emit the token/cost metrics.
    pub metrics: bool,
}

/// What an export run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ExportSummary {
    pub messages_seen: u64,
    pub messages_exported: u64,
    pub messages_already_exported: u64,
    pub spans: u64,
    pub batches: u64,
    pub dry_run: bool,
}

impl ExportSummary {
    /// True when nothing changed since the last run.
    pub fn is_empty(&self) -> bool {
        self.messages_exported == 0
    }
}

/// Run a one-shot export over every available analyzer.
pub async fn run_export(options: ExportOptions) -> Result<()> {
    let config = crate::config::Config::load()?.unwrap_or_default();
    let otel = config.otel.clone();
    let cutoff = cutoff_from_days(options.days.unwrap_or(otel.backfill_days));

    let registry = crate::create_analyzer_registry();
    let groups = load_messages(&registry, &otel, options.analyzer.as_deref(), cutoff)?;

    if groups.is_empty() {
        println!("Nothing to export: no analyzer had usage data to parse.");
        return Ok(());
    }

    let exporter = Exporter::new(&otel, options.endpoint.as_deref(), options.metrics)?;
    if !options.dry_run {
        exporter.validate()?;
    }

    let mut state = state::ExportStateHandle::open(exporter.traces_endpoint())?;
    let summary = exporter.send(&groups, &mut state, &options).await?;

    if summary.dry_run {
        println!(
            "Dry run: {} messages would be exported as {} spans in {} batch(es) to {}",
            summary.messages_exported,
            summary.spans,
            summary.batches,
            exporter.traces_endpoint()
        );
    } else {
        println!(
            "Exported {} messages as {} spans in {} batch(es) to {}",
            summary.messages_exported,
            summary.spans,
            summary.batches,
            exporter.traces_endpoint()
        );
    }
    if summary.messages_already_exported > 0 {
        println!(
            "Skipped {} messages that were already exported.",
            summary.messages_already_exported
        );
    }

    Ok(())
}

/// One analyzer's parsed output, ready to export.
pub(crate) struct LoadedAnalyzer {
    pub name: String,
    pub messages: Vec<ConversationMessage>,
    pub tool_calls: Vec<ToolCall>,
}

/// Load messages (and, when asked for, per-call tool records) for each selected
/// analyzer.
pub(crate) fn load_messages(
    registry: &AnalyzerRegistry,
    otel: &OtelConfig,
    only: Option<&str>,
    cutoff: Option<DateTime<Utc>>,
) -> Result<Vec<LoadedAnalyzer>> {
    if let Some(name) = only
        && registry.get_analyzer_by_display_name(name).is_none()
    {
        let mut available: Vec<&'static str> = registry
            .available_analyzers()
            .iter()
            .map(|analyzer| analyzer.display_name())
            .collect();
        available.sort_unstable();
        bail!(
            "Unknown analyzer {name:?}. Available: {}",
            if available.is_empty() {
                "none (no usage data found)".to_string()
            } else {
                available.join(", ")
            }
        );
    }

    let selected: Vec<_> = registry
        .available_analyzers_with_sources()
        .into_iter()
        .filter(|(analyzer, _)| {
            let name = analyzer.display_name();
            if otel.excludes(name) {
                return false;
            }
            only.is_none_or(|wanted| wanted.eq_ignore_ascii_case(name))
        })
        .collect();

    // Parse in parallel; this is the same work the upload path does.
    let pool = rayon::ThreadPoolBuilder::new()
        .build()
        .context("Failed to create rayon threadpool")?;
    let want_tool_calls = otel.include_tool_spans;
    let parsed: Vec<LoadedAnalyzer> = pool.install(|| {
        use rayon::prelude::*;
        selected
            .into_par_iter()
            .filter_map(|(analyzer, sources)| {
                let name = analyzer.display_name().to_string();
                let stats = match analyzer.get_stats_with_sources(sources.clone()) {
                    Ok(stats) => stats,
                    Err(error) => {
                        eprintln!("⚠️  {name}: {error}");
                        return None;
                    }
                };
                let tool_calls = if want_tool_calls {
                    match analyzer.parse_tool_calls(&sources) {
                        Ok(calls) => calls,
                        Err(error) => {
                            eprintln!("⚠️  {name}: tool calls unavailable: {error}");
                            Vec::new()
                        }
                    }
                } else {
                    Vec::new()
                };
                Some(LoadedAnalyzer {
                    name,
                    messages: stats.messages,
                    tool_calls,
                })
            })
            .collect()
    });

    Ok(parsed
        .into_iter()
        .map(|loaded| {
            let messages = match cutoff {
                Some(cutoff) => loaded
                    .messages
                    .into_iter()
                    .filter(|message| message.date >= cutoff)
                    .collect::<Vec<_>>(),
                None => loaded.messages,
            };
            LoadedAnalyzer { messages, ..loaded }
        })
        .filter(|loaded| !loaded.messages.is_empty())
        .collect())
}

/// Timestamp cutoff for `--days`, or `None` for "all history".
pub(crate) fn cutoff_from_days(days: i64) -> Option<DateTime<Utc>> {
    if days <= 0 {
        return None;
    }
    Utc::now().checked_sub_signed(chrono::Duration::days(days))
}

/// A configured sender: endpoints, headers, resource, and the HTTP client.
pub struct Exporter {
    otel: OtelConfig,
    traces: config::OtlpTarget,
    metrics: Option<config::OtlpTarget>,
    client: client::OtlpClient,
    resource: encode::Resource,
}

impl Exporter {
    /// Build an exporter. Metrics are only wired up when asked for, so an
    /// traces-only setup never resolves or touches a metrics endpoint.
    pub fn new(
        otel: &OtelConfig,
        endpoint_override: Option<&str>,
        with_metrics: bool,
    ) -> Result<Self> {
        let traces = otel.resolve_traces(endpoint_override);
        let metrics = with_metrics.then(|| otel.resolve_metrics(endpoint_override));
        let client = client::OtlpClient::new(&traces, metrics.as_ref())?;
        Ok(Self {
            otel: otel.clone(),
            resource: encode::resource_for(&traces.service_name),
            traces,
            metrics,
            client,
        })
    }

    pub fn validate(&self) -> Result<()> {
        self.traces.validate()?;
        config::validate_protocol(&self.otel.protocol)?;
        if let Some(metrics) = &self.metrics {
            metrics.validate()?;
        }
        Ok(())
    }

    pub fn traces_endpoint(&self) -> &str {
        &self.traces.endpoint
    }

    /// Build, send, and record one batch set of already-loaded messages.
    pub async fn send(
        &self,
        groups: &[LoadedAnalyzer],
        state: &mut state::ExportStateHandle,
        options: &ExportOptions,
    ) -> Result<ExportSummary> {
        let mut summary = ExportSummary {
            dry_run: options.dry_run,
            ..Default::default()
        };

        let mut trace_requests: Vec<encode::ExportTraceRequest> = Vec::new();
        let mut metric_requests: Vec<encode::ExportMetricsRequest> = Vec::new();
        let mut current: Vec<spans::TurnSpans> = Vec::new();
        let mut current_spans = 0usize;
        let batch_size = self.otel.max_batch_size.max(1);

        for group in groups {
            let analyzer_name = &group.name;
            let messages = &group.messages;
            summary.messages_seen += messages.len() as u64;
            let fresh: Vec<ConversationMessage> = messages
                .iter()
                .filter(|message| !state.is_exported(&message.global_hash))
                .cloned()
                .collect();
            summary.messages_already_exported += (messages.len() - fresh.len()) as u64;
            if fresh.is_empty() {
                continue;
            }

            for message in &fresh {
                state.record(&message.global_hash, message.date);
            }
            summary.messages_exported += fresh.len() as u64;

            if self.metrics.is_some() {
                let metrics = metrics::build_metrics(analyzer_name, &fresh);
                if !metrics.is_empty() {
                    metric_requests.push(encode::metrics_request(&self.resource, metrics));
                }
            }

            // Tool calls are exported once each, and only for conversations
            // whose messages are in this batch: re-parsing one file must not
            // dump the rest of that analyzer's history.
            let fresh_calls: Vec<ToolCall> = if self.otel.include_tool_spans {
                let conversations: HashSet<&str> = fresh
                    .iter()
                    .map(|message| message.conversation_hash.as_str())
                    .collect();
                let calls: Vec<ToolCall> = group
                    .tool_calls
                    .iter()
                    .filter(|call| conversations.contains(call.conversation_hash.as_str()))
                    .filter(|call| !state.is_exported(&tool_state_key(analyzer_name, call)))
                    .cloned()
                    .collect();
                for call in &calls {
                    state.record(&tool_state_key(analyzer_name, call), call.timestamp);
                }
                calls
            } else {
                Vec::new()
            };

            let turns = spans::build_turns(
                analyzer_name,
                &fresh,
                &fresh_calls,
                self.otel.include_tool_spans,
            );
            for turn in turns {
                summary.spans += turn.spans.len() as u64;
                current_spans += turn.spans.len();
                current.push(turn);
                if current_spans >= batch_size {
                    trace_requests.push(request_for(&self.resource, std::mem::take(&mut current)));
                    current_spans = 0;
                }
            }
        }

        if !current.is_empty() {
            trace_requests.push(request_for(&self.resource, current));
        }
        summary.batches = trace_requests.len() as u64;

        if options.dry_run {
            print_dry_run(
                &trace_requests,
                &metric_requests,
                options.pretty,
                summary.spans,
            )?;
            return Ok(summary);
        }

        for request in &trace_requests {
            self.client.post_traces(request).await?;
        }
        for request in &metric_requests {
            self.client.post_metrics(request).await?;
        }
        state.commit()?;

        Ok(summary)
    }
}

fn print_dry_run(
    traces: &[encode::ExportTraceRequest],
    metrics: &[encode::ExportMetricsRequest],
    pretty: bool,
    total_spans: u64,
) -> Result<()> {
    let Some(first) = traces.first() else {
        if let Some(metrics) = metrics.first() {
            println!("{}", render(metrics, pretty)?);
        }
        return Ok(());
    };
    println!("{}", render(first, pretty)?);
    if traces.len() > 1 || !metrics.is_empty() {
        eprintln!(
            "(showing 1 of {} trace batch(es) and {} metrics batch(es); {} spans total)",
            traces.len(),
            metrics.len(),
            total_spans
        );
    }
    Ok(())
}

fn render<T: serde::Serialize>(value: &T, pretty: bool) -> Result<String> {
    let rendered = if pretty {
        simd_json::to_string_pretty(value)
    } else {
        simd_json::to_string(value)
    };
    rendered.context("Failed to serialize OTLP payload")
}

/// State key for a tool call, namespaced so it cannot collide with a message
/// hash that happens to look similar.
fn tool_state_key(analyzer: &str, call: &ToolCall) -> String {
    format!("tool:{analyzer}:{}", call.key())
}

/// Flatten turns into one OTLP request, keeping every trace intact.
fn request_for(
    resource: &encode::Resource,
    mut turns: Vec<spans::TurnSpans>,
) -> encode::ExportTraceRequest {
    // Deterministic ordering keeps re-exports byte-identical, which makes
    // diffing two runs (and the tests) meaningful.
    turns.sort_by(|a, b| a.trace_id.cmp(&b.trace_id));
    let spans = turns
        .into_iter()
        .flat_map(|turn| turn.spans)
        .collect::<Vec<_>>();
    encode::request_from_spans(resource, spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cutoff_zero_means_everything() {
        assert!(cutoff_from_days(0).is_none());
        assert!(cutoff_from_days(-5).is_none());
    }

    #[test]
    fn cutoff_days_are_in_the_past() {
        let cutoff = cutoff_from_days(2).expect("cutoff");
        assert!(cutoff < Utc::now());
        assert!(cutoff > Utc::now() - chrono::Duration::days(3));
    }

    use crate::types::{Application, ConversationMessage, MessageRole, Stats};
    use axum::Router;
    use axum::body::Bytes;
    use axum::extract::State;
    use axum::routing::post;
    use chrono::TimeZone;
    use parking_lot::Mutex;
    use simd_json::prelude::*;
    use std::sync::Arc;

    type Captured = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

    async fn collector_handler(
        State(captured): State<Captured>,
        uri: axum::http::Uri,
        body: Bytes,
    ) -> &'static str {
        captured
            .lock()
            .push((uri.path().to_string(), body.to_vec()));
        "{}"
    }

    /// Spin up a fake collector on a random port and collect what it receives.
    async fn spawn_collector() -> (String, Captured) {
        let captured: Captured = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/v1/traces", post(collector_handler))
            .route("/v1/metrics", post(collector_handler))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}"), captured)
    }

    fn message(offset_secs: i64, role: MessageRole, id: &str) -> ConversationMessage {
        ConversationMessage {
            application: Application::CodexCli,
            date: Utc.with_ymd_and_hms(2026, 5, 6, 7, 8, 0).unwrap()
                + chrono::Duration::seconds(offset_secs),
            project_hash: "project".to_string(),
            project_path: None,
            conversation_hash: "conv-1".to_string(),
            local_hash: None,
            global_hash: format!("hash-{id}"),
            model: Some("gpt-5.6-codex".to_string()),
            stats: match role {
                MessageRole::User => Stats::default(),
                MessageRole::Assistant => Stats {
                    input_tokens: 100,
                    output_tokens: 10,
                    cost: 0.02,
                    ..Stats::default()
                },
            },
            role,
            uuid: None,
            session_name: None,
        }
    }

    fn groups() -> Vec<LoadedAnalyzer> {
        vec![LoadedAnalyzer {
            name: "Codex CLI".to_string(),
            messages: vec![
                message(0, MessageRole::User, "u1"),
                message(5, MessageRole::Assistant, "a1"),
            ],
            tool_calls: Vec::new(),
        }]
    }

    fn test_state_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "splitrail-otel-e2e-{name}-{}.json",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        state::set_test_state_path(path.clone());
        path
    }

    #[tokio::test]
    async fn sends_traces_and_metrics_to_a_collector() {
        let state_path = test_state_path("send");
        let (endpoint, captured) = spawn_collector().await;
        let otel = OtelConfig {
            endpoint,
            ..Default::default()
        };

        let exporter = Exporter::new(&otel, None, true).expect("exporter");
        exporter.validate().expect("valid");

        let mut state = state::ExportStateHandle::open("http://127.0.0.1:1").expect("state");
        let summary = exporter
            .send(&groups(), &mut state, &ExportOptions::default())
            .await
            .expect("send");

        assert_eq!(summary.messages_exported, 2);
        assert_eq!(summary.spans, 2);
        assert_eq!(summary.batches, 1);

        // Give the collector a moment to record the requests.
        for _ in 0..50 {
            if captured.lock().len() >= 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        let captured = captured.lock().clone();
        let traces = captured
            .iter()
            .find(|(path, _)| path == "/v1/traces")
            .expect("traces request");
        let metrics = captured
            .iter()
            .find(|(path, _)| path == "/v1/metrics")
            .expect("metrics request");

        let mut bytes = traces.1.clone();
        let payload: simd_json::OwnedValue = simd_json::from_slice(&mut bytes).expect("json");
        assert_eq!(
            payload
                .get("resourceSpans")
                .and_then(|value| value.as_array())
                .map(Vec::len),
            Some(1)
        );
        let spans = payload
            .get("resourceSpans")
            .and_then(|value| value.as_array())
            .and_then(|value| value.first())
            .and_then(|value| value.get("scopeSpans"))
            .and_then(|value| value.as_array())
            .and_then(|value| value.first())
            .and_then(|value| value.get("spans"))
            .and_then(|value| value.as_array())
            .expect("spans array");
        assert_eq!(spans.len(), 2);

        let mut metric_bytes = metrics.1.clone();
        let metric_payload: simd_json::OwnedValue =
            simd_json::from_slice(&mut metric_bytes).expect("json");
        let rendered = simd_json::to_string(&metric_payload).expect("serialize");
        assert!(rendered.contains("gen_ai.client.token.usage"));
        assert!(rendered.contains("splitrail.cost.usd"));

        let _ = std::fs::remove_file(state_path);
    }

    #[tokio::test]
    async fn second_run_does_not_re_export() {
        let state_path = test_state_path("idempotent");
        let (endpoint, captured) = spawn_collector().await;
        let otel = OtelConfig {
            endpoint,
            ..Default::default()
        };
        let exporter = Exporter::new(&otel, None, false).expect("exporter");
        let mut state = state::ExportStateHandle::open("http://127.0.0.1:1").expect("state");

        let first = exporter
            .send(&groups(), &mut state, &ExportOptions::default())
            .await
            .expect("first send");
        assert_eq!(first.messages_exported, 2);

        // Same transcript, parsed again (as a live watcher would).
        let second = exporter
            .send(&groups(), &mut state, &ExportOptions::default())
            .await
            .expect("second send");
        assert_eq!(second.messages_exported, 0);
        assert_eq!(second.messages_already_exported, 2);
        assert_eq!(second.batches, 0);

        let _ = captured;
        let _ = std::fs::remove_file(state_path);
    }

    #[tokio::test]
    async fn dry_run_sends_nothing() {
        let state_path = test_state_path("dry-run");
        let (endpoint, captured) = spawn_collector().await;
        let otel = OtelConfig {
            endpoint,
            ..Default::default()
        };
        let exporter = Exporter::new(&otel, None, false).expect("exporter");
        let mut state = state::ExportStateHandle::open("http://127.0.0.1:1").expect("state");

        let summary = exporter
            .send(
                &groups(),
                &mut state,
                &ExportOptions {
                    dry_run: true,
                    ..Default::default()
                },
            )
            .await
            .expect("dry run");

        assert!(summary.dry_run);
        assert_eq!(summary.spans, 2);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(captured.lock().is_empty(), "no request should be sent");

        let _ = std::fs::remove_file(state_path);
    }

    #[test]
    fn summary_reports_emptiness() {
        assert!(ExportSummary::default().is_empty());
        assert!(
            !ExportSummary {
                messages_exported: 1,
                ..Default::default()
            }
            .is_empty()
        );
    }
}
