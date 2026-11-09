//! Live export: follow transcript changes and ship them as they land.
//!
//! The TUI already solves "what changed on disk" with [`crate::watcher`], so the
//! daemon reuses it rather than inventing a second watcher. Where it differs is
//! debouncing: an agent appends to a transcript while it is still writing, and a
//! half-written record would be exported under one hash and then again under
//! another once it settles. So a message is only eligible once it is
//! `settle_secs` old, and files with younger records stay on a pending list that
//! is retried on the next tick. On shutdown everything pending is flushed, since
//! a user pressing Ctrl-C is not going to change the file again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use std::future::Future;

use crate::analyzer::{Analyzer, DataSource};
use crate::config::Config;
use crate::watcher::{FileWatcher, WatcherEvent};

use super::config::validate_protocol;
use super::state::ExportStateHandle;
use super::{ExportOptions, Exporter, load_messages};

/// Default settle window: how old a message must be before it is exported.
const DEFAULT_SETTLE_SECS: u64 = 10;

/// How often the watcher queue and the pending list are drained.
const TICK: Duration = Duration::from_secs(1);

/// Options for the long-running exporter.
#[derive(Debug, Clone, Default)]
pub struct ServeOptions {
    /// Override the configured endpoint.
    pub endpoint: Option<String>,
    /// Skip the initial sweep of existing history.
    pub no_backfill: bool,
    /// How much history the initial sweep covers. `None` uses the config value.
    pub backfill_days: Option<i64>,
    /// Also emit token/cost metrics.
    pub metrics: bool,
    /// Exit after the initial sweep instead of following changes (for tests and
    /// cron-style use).
    pub once: bool,
}

/// How long a message has to sit still before it is exported.
fn settle_duration(config_value: u64) -> Duration {
    Duration::from_secs(if config_value == 0 {
        DEFAULT_SETTLE_SECS
    } else {
        config_value
    })
}

/// Run the live exporter until interrupted.
pub async fn run_serve(options: ServeOptions) -> Result<()> {
    run_serve_with_shutdown(options, async { Ok(tokio::signal::ctrl_c().await?) }).await
}

/// Run the live exporter until the caller requests shutdown.
pub async fn run_serve_with_shutdown(
    options: ServeOptions,
    shutdown: impl Future<Output = Result<()>>,
) -> Result<()> {
    let startup_cutoff = Utc::now();
    tokio::pin!(shutdown);
    let config = Config::load()?.unwrap_or_default();
    let otel = config.otel.clone();
    let exporter = Exporter::new(&otel, options.endpoint.as_deref(), options.metrics)?;
    exporter.validate()?;
    validate_protocol(&otel.protocol)?;

    let registry = crate::create_analyzer_registry();
    let mut state = ExportStateHandle::open(exporter.traces_endpoint())?;
    let settle = settle_duration(otel.settle_secs);

    println!("OTel exporter → {}", exporter.traces_endpoint());
    println!(
        "Following transcripts (settle window {}s). Press Ctrl-C to stop.",
        settle.as_secs()
    );

    if !options.no_backfill {
        let cutoff = super::cutoff_from_days(options.backfill_days.unwrap_or(otel.backfill_days));
        let groups = load_messages(&registry, &otel, None, cutoff)?;
        let summary = exporter
            .send(&groups, &mut state, &export_options(&options))
            .await?;
        println!(
            "Backfilled {} messages ({} spans).",
            summary.messages_exported, summary.spans
        );
    }

    if options.once {
        state.commit()?;
        return Ok(());
    }

    let watcher = FileWatcher::new(&registry)?;
    let mut pending: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut touched_since_flush = false;
    loop {
        tokio::select! {
            result = &mut shutdown => {
                result?;
                drain_events(&watcher, &otel, &mut pending);
                println!("\nStopping; flushing {} pending file(s).", pending.len());
                flush_pending(
                    &exporter, &registry, &mut state, &mut pending,
                    Duration::ZERO, &options,
                    options.no_backfill.then_some(startup_cutoff),
                ).await?;
                anyhow::ensure!(pending.is_empty(), "OTel shutdown left {} file(s) pending after parse or discovery failures", pending.len());
                state.commit()?;
                return Ok(());
            },
            _ = tokio::time::sleep(TICK) => {
                let saw_event = drain_events(&watcher, &otel, &mut pending);
                if saw_event || !pending.is_empty() {
                    match flush_pending(
                        &exporter, &registry, &mut state, &mut pending, settle, &options,
                        options.no_backfill.then_some(startup_cutoff),
                    ).await {
                        Ok(summary) if !summary.is_empty() => touched_since_flush = true,
                        Ok(_) => {}
                        Err(error) => eprintln!("⚠️  OTel follower: {error:#}"),
                    }
                }
                if touched_since_flush && pending.is_empty() {
                    state.commit()?;
                    touched_since_flush = false;
                }
            },
        }
    }
}

fn drain_events(
    watcher: &FileWatcher,
    otel: &super::OtelConfig,
    pending: &mut BTreeMap<PathBuf, String>,
) -> bool {
    let mut saw_event = false;
    while let Some(event) = watcher.try_recv() {
        match event {
            WatcherEvent::FileChanged(analyzer, path) => {
                if !otel.excludes(&analyzer) {
                    pending.insert(path, analyzer);
                    saw_event = true;
                }
            }
            WatcherEvent::FileDeleted(_, path) => {
                pending.remove(&path);
            }
            WatcherEvent::Error(error) => eprintln!("⚠️  watcher: {error}"),
        }
    }
    saw_event
}

fn export_options(options: &ServeOptions) -> ExportOptions {
    ExportOptions {
        analyzer: None,
        days: options.backfill_days,
        dry_run: false,
        pretty: false,
        endpoint: options.endpoint.clone(),
        metrics: options.metrics,
    }
}

async fn flush_pending(
    exporter: &Exporter,
    registry: &crate::analyzer::AnalyzerRegistry,
    state: &mut ExportStateHandle,
    pending: &mut BTreeMap<PathBuf, String>,
    settle: Duration,
    options: &ServeOptions,
    startup_cutoff: Option<chrono::DateTime<Utc>>,
) -> Result<super::ExportSummary> {
    if pending.is_empty() {
        return Ok(super::ExportSummary::default());
    }

    let cutoff =
        Utc::now() - chrono::Duration::from_std(settle).unwrap_or(chrono::Duration::zero());
    let mut groups: Vec<super::LoadedAnalyzer> = Vec::new();
    let mut still_pending = BTreeSet::new();
    for (path, analyzer_name) in pending.iter() {
        let Some(analyzer) = registry.get_analyzer_by_display_name(analyzer_name) else {
            continue;
        };
        let sources = match analyzer.discover_data_sources() {
            Ok(sources) => sources,
            Err(error) => {
                eprintln!("⚠️  OTel discovery: {error:#}");
                still_pending.insert(path.clone());
                continue;
            }
        };
        let Some(source) = resolve_event_source(analyzer, &sources, path) else {
            still_pending.insert(path.clone());
            continue;
        };
        let source_set = if analyzer.requires_full_reload_for_source_change() {
            sources
        } else {
            vec![source]
        };
        let stats = match analyzer.get_stats_with_sources(source_set.clone()) {
            Ok(stats) => stats,
            Err(error) => {
                eprintln!("⚠️  OTel parse: {error:#}");
                still_pending.insert(path.clone());
                continue;
            }
        };
        let messages = stats.messages;
        if messages.iter().any(|message| message.date > cutoff) {
            still_pending.insert(path.clone());
        }
        let eligible_hashes = messages
            .iter()
            .filter(|message| {
                message.date <= cutoff && eligible_since_startup(message.date, startup_cutoff)
            })
            .map(|message| message.global_hash.clone())
            .collect();
        let tool_calls = if exporter.tools_enabled() {
            match analyzer.parse_tool_calls(&source_set) {
                Ok(calls) => calls,
                Err(error) => {
                    eprintln!("⚠️  OTel tool parsing: {error:#}");
                    still_pending.insert(path.clone());
                    continue;
                }
            }
        } else {
            Vec::new()
        };
        let eligible_tool_keys = tool_calls
            .iter()
            .filter(|call| {
                call.timestamp <= cutoff && eligible_since_startup(call.timestamp, startup_cutoff)
            })
            .map(|call| super::tool_state_key(analyzer_name, call))
            .collect();
        if !messages.is_empty() || !tool_calls.is_empty() {
            groups.push(super::LoadedAnalyzer {
                name: analyzer_name.clone(),
                messages,
                tool_calls,
                eligible_hashes: Some(eligible_hashes),
                eligible_tool_keys: Some(eligible_tool_keys),
            });
        }
    }
    let summary = if groups.is_empty() {
        super::ExportSummary::default()
    } else {
        exporter
            .send(&groups, state, &export_options(options))
            .await?
    };
    pending.retain(|path, _| still_pending.contains(path));
    Ok(summary)
}

fn resolve_event_source(
    analyzer: &dyn Analyzer,
    sources: &[DataSource],
    event_path: &Path,
) -> Option<DataSource> {
    if let Some(source) = sources
        .iter()
        .filter(|source| event_path.starts_with(&source.path))
        .max_by_key(|source| source.path.components().count())
    {
        return Some(source.clone());
    }
    if analyzer.is_valid_data_path(event_path) {
        return Some(DataSource {
            path: event_path.to_path_buf(),
        });
    }
    sources
        .iter()
        .filter(|source| source.path.starts_with(event_path))
        .max_by_key(|source| source.path.components().count())
        .cloned()
}

fn eligible_since_startup(
    timestamp: chrono::DateTime<Utc>,
    startup_cutoff: Option<chrono::DateTime<Utc>>,
) -> bool {
    startup_cutoff.is_none_or(|startup| timestamp >= startup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_cutoff_excludes_older_records_and_includes_boundary() {
        let startup = Utc::now();
        assert!(!eligible_since_startup(
            startup - chrono::Duration::nanoseconds(1),
            Some(startup)
        ));
        assert!(eligible_since_startup(startup, Some(startup)));
        assert!(eligible_since_startup(
            startup - chrono::Duration::days(1),
            None
        ));
    }

    #[test]
    fn settle_window_defaults_when_unset() {
        assert_eq!(settle_duration(0).as_secs(), DEFAULT_SETTLE_SECS);
        assert_eq!(settle_duration(30).as_secs(), 30);
    }

    #[test]
    fn serve_defaults_were_opt_in() {
        let options = ServeOptions::default();
        assert!(!options.metrics);
        assert!(!options.no_backfill);
        assert!(!options.once);
    }

    #[test]
    fn export_options_are_not_a_dry_run() {
        let options = serve_options_with_endpoint();
        let export = export_options(&options);
        assert!(!export.dry_run);
        assert!(export.analyzer.is_none());
    }

    fn serve_options_with_endpoint() -> ServeOptions {
        ServeOptions {
            endpoint: Some("http://collector:4318".to_string()),
            ..ServeOptions::default()
        }
    }
}
