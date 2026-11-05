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
            _ = tokio::signal::ctrl_c() => {
                println!("\nStopping; flushing {} pending file(s).", pending.len());
                flush_pending(
                    &exporter,
                    &registry,
                    &mut state,
                    &mut pending,
                    Duration::ZERO,
                    &options,
                )
                .await?;
                state.commit()?;
                return Ok(());
            }
            _ = tokio::time::sleep(TICK) => {
                let mut saw_event = false;
                while let Some(event) = watcher.try_recv() {
                    match event {
                        WatcherEvent::FileChanged(analyzer, path) => {
                            if otel.excludes(&analyzer) {
                                continue;
                            }
                            let Some(analyzer_impl) = registry.get_analyzer_by_display_name(&analyzer) else {
                                continue;
                            };
                            let sources = analyzer_impl.discover_data_sources().unwrap_or_default();
                            if resolve_event_source(analyzer_impl, &sources, &path).is_none() {
                                continue;
                            }
                            pending.insert(path, analyzer);
                            saw_event = true;
                        }
                        WatcherEvent::FileDeleted(_, path) => {
                            pending.remove(&path);
                        }
                        WatcherEvent::Error(error) => eprintln!("⚠️  watcher: {error}"),
                    }
                }
                if saw_event || !pending.is_empty() {
                    let summary = flush_pending(&exporter, &registry, &mut state, &mut pending, settle, &options).await?;
                    if !summary.is_empty() {
                        touched_since_flush = true;
                    }
                }
                if touched_since_flush && pending.is_empty() {
                    state.commit()?;
                    touched_since_flush = false;
                }
            }
        }
    }
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

/// Re-parse the pending files, export settled messages, and keep files whose
/// newest message is still fresh for another tick.
async fn flush_pending(
    exporter: &Exporter,
    registry: &crate::analyzer::AnalyzerRegistry,
    state: &mut ExportStateHandle,
    pending: &mut BTreeMap<PathBuf, String>,
    settle: Duration,
    options: &ServeOptions,
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
        let sources = analyzer.discover_data_sources()?;
        let Some(source) = resolve_event_source(analyzer, &sources, path) else {
            continue;
        };
        let source_set = if analyzer.requires_full_reload_for_source_change() {
            sources.clone()
        } else {
            vec![source]
        };
        let stats = analyzer.get_stats_with_sources(source_set.clone())?;
        let messages = stats.messages;
        let (settled, fresh): (Vec<_>, Vec<_>) =
            messages.iter().partition(|message| message.date <= cutoff);
        if !fresh.is_empty() {
            still_pending.insert(path.clone());
        }
        let eligible_hashes = settled
            .iter()
            .map(|message| message.global_hash.clone())
            .collect();
        let tool_calls = if exporter.tools_enabled() {
            analyzer.parse_tool_calls(&source_set)?
        } else {
            Vec::new()
        };
        let eligible_tool_keys = tool_calls
            .iter()
            .filter(|call| call.timestamp <= cutoff)
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
    *pending = pending
        .iter()
        .filter(|(path, _)| still_pending.contains(*path))
        .map(|(path, analyzer)| (path.clone(), analyzer.clone()))
        .collect();
    if groups.is_empty() {
        return Ok(super::ExportSummary::default());
    }
    exporter
        .send(&groups, state, &export_options(options))
        .await
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

#[cfg(test)]
mod tests {
    use super::*;

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
