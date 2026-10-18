use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use axum::extract::{Query, Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Local, Timelike, Utc};
use serde::{Deserialize, Serialize};

use crate::create_analyzer_registry;
use crate::types::{MessageRole, MultiAnalyzerStats};

mod projects;

use projects::Projects;

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct HourlyUsage {
    date: String,
    hour: u8,
    model: String,
    project: String,
    tool: String,
    session: String,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    reasoning_tokens: u64,
    cost: f64,
    messages: u32,
    tool_calls: u32,
    #[serde(default)]
    files_read: u64,
    #[serde(default)]
    files_edited: u64,
    #[serde(default)]
    files_added: u64,
    #[serde(default)]
    terminal_commands: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionInfo {
    id: String,
    name: Option<String>,
    tool: String,
    project: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    #[serde(default)]
    active_minutes: u32,
    #[serde(skip)]
    timestamps: Vec<DateTime<Utc>>,
}

/// A project assembled from several paths (worktrees, moves, subdirectories).
#[derive(Deserialize, Serialize)]
struct ProjectInfo {
    id: String,
    paths: Vec<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageSnapshot {
    scanned_at: DateTime<Utc>,
    rows: Vec<HourlyUsage>,
    sessions: Vec<SessionInfo>,
    projects: Vec<ProjectInfo>,
}

fn active_minutes(timestamps: &mut [DateTime<Utc>]) -> u32 {
    timestamps.sort_unstable();
    let seconds: i64 = timestamps
        .windows(2)
        .map(|pair| (pair[1] - pair[0]).num_seconds())
        .filter(|seconds| *seconds > 0 && *seconds < 5 * 60)
        .sum();
    u32::try_from((seconds.max(60) + 59) / 60).unwrap_or(u32::MAX)
}

fn usage_snapshot(stats: &MultiAnalyzerStats) -> UsageSnapshot {
    let messages = || {
        stats.analyzer_stats.iter().flat_map(|analyzer| {
            analyzer
                .messages
                .iter()
                .map(|message| (analyzer.analyzer_name.as_str(), message))
        })
    };
    // Analyzer project hashes describe where a session started, so only each
    // session's earliest path may link its hash to other paths.
    let mut starts: HashMap<(&str, &str), (DateTime<Utc>, &str, &str)> = HashMap::new();
    for (tool, message) in messages() {
        let Some(path) = message.project_path.as_deref() else {
            continue;
        };
        let start = (message.date, message.project_hash.as_str(), path);
        starts
            .entry((tool, &message.conversation_hash))
            .and_modify(|earliest| {
                if start.0 < earliest.0 {
                    *earliest = start;
                }
            })
            .or_insert(start);
    }
    let projects = Projects::group(
        starts.values().map(|&(_, hash, path)| (hash, path)),
        messages().filter_map(|(_, message)| message.project_path.as_deref()),
        dirs::home_dir().as_deref(),
    );

    type HourKey = (String, u8, String, String, String, String);
    let mut hours: BTreeMap<HourKey, HourlyUsage> = BTreeMap::new();
    let mut sessions: HashMap<(&str, &str), SessionInfo> = HashMap::new();
    for (tool, message) in messages() {
        let project = projects.id(&message.project_hash, message.project_path.as_deref());
        // Sessions span user and assistant messages; the earliest message names the project.
        let session = sessions
            .entry((tool, &message.conversation_hash))
            .or_insert_with(|| SessionInfo {
                id: format!("{tool}:{}", message.conversation_hash),
                name: None,
                tool: tool.to_string(),
                project: project.to_string(),
                start: message.date,
                end: message.date,
                active_minutes: 0,
                timestamps: Vec::new(),
            });
        if message.date < session.start {
            session.start = message.date;
            session.project = project.to_string();
        }
        session.end = session.end.max(message.date);
        session.timestamps.push(message.date);
        if session.name.is_none() {
            session.name.clone_from(&message.session_name);
        }
        if message.role != MessageRole::Assistant {
            continue;
        }

        let local_time = message.date.with_timezone(&Local);
        let date = local_time.format("%Y-%m-%d").to_string();
        let hour = local_time.hour() as u8;
        let model = message.model.as_deref().unwrap_or("Unknown model");
        let row = hours
            .entry((
                date.clone(),
                hour,
                model.to_string(),
                project.to_string(),
                tool.to_string(),
                message.conversation_hash.clone(),
            ))
            .or_insert_with(|| HourlyUsage {
                date,
                hour,
                model: model.to_string(),
                project: project.to_string(),
                tool: tool.to_string(),
                session: format!("{tool}:{}", message.conversation_hash),
                ..Default::default()
            });
        row.input_tokens = row.input_tokens.saturating_add(message.stats.input_tokens);
        row.output_tokens = row
            .output_tokens
            .saturating_add(message.stats.output_tokens);
        row.cached_tokens = row
            .cached_tokens
            .saturating_add(message.stats.cached_tokens);
        row.reasoning_tokens = row
            .reasoning_tokens
            .saturating_add(message.stats.reasoning_tokens);
        row.cost += message.stats.cost;
        row.messages = row.messages.saturating_add(1);
        row.tool_calls = row.tool_calls.saturating_add(message.stats.tool_calls);
        row.files_read = row.files_read.saturating_add(message.stats.files_read);
        row.files_edited = row.files_edited.saturating_add(message.stats.files_edited);
        row.files_added = row.files_added.saturating_add(message.stats.files_added);
        row.terminal_commands = row
            .terminal_commands
            .saturating_add(message.stats.terminal_commands);
    }

    let rows: Vec<HourlyUsage> = hours
        .into_values()
        .filter(|row| {
            row.input_tokens > 0
                || row.output_tokens > 0
                || row.cached_tokens > 0
                || row.reasoning_tokens > 0
                || row.cost > 0.0
        })
        .collect();
    let used_sessions: HashSet<&str> = rows.iter().map(|row| row.session.as_str()).collect();
    let mut sessions: Vec<SessionInfo> = sessions
        .into_values()
        .filter(|session| used_sessions.contains(session.id.as_str()))
        .map(|mut session| {
            session.active_minutes = active_minutes(&mut session.timestamps);
            session
        })
        .collect();
    sessions.sort_by(|left, right| {
        left.start
            .cmp(&right.start)
            .then_with(|| left.id.cmp(&right.id))
    });
    let used_projects: HashSet<&str> = rows
        .iter()
        .map(|row| row.project.as_str())
        .chain(sessions.iter().map(|session| session.project.as_str()))
        .collect();
    let projects: Vec<ProjectInfo> = projects
        .paths
        .into_iter()
        .filter(|(id, _)| used_projects.contains(id.as_str()))
        .map(|(id, paths)| ProjectInfo {
            id,
            paths: paths.into_iter().collect(),
        })
        .collect();
    UsageSnapshot {
        scanned_at: Utc::now(),
        rows,
        sessions,
        projects,
    }
}

struct CachedUsage {
    snapshot: Arc<UsageSnapshot>,
    completed_at: Instant,
    from_disk: bool,
}

#[derive(Default)]
struct UsageState {
    cached: parking_lot::RwLock<Option<CachedUsage>>,
    scan: tokio::sync::Mutex<()>,
    cache_path: Option<PathBuf>,
    model_aliases: HashMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageResponse<'a> {
    #[serde(flatten)]
    snapshot: &'a UsageSnapshot,
    model_aliases: &'a HashMap<String, String>,
}

fn cache_timezone() -> String {
    std::env::var("TZ")
        .ok()
        .or_else(|| iana_time_zone::get_timezone().ok())
        .unwrap_or_else(|| Local::now().offset().to_string())
}

fn read_snapshot(path: &Path) -> Result<UsageSnapshot> {
    let mut bytes = std::fs::read(path)?;
    let (version, timezone, snapshot): (String, String, UsageSnapshot) =
        simd_json::serde::from_slice(&mut bytes)?;
    anyhow::ensure!(
        version == env!("CARGO_PKG_VERSION") && timezone == cache_timezone(),
        "Web cache version or timezone changed"
    );
    Ok(snapshot)
}

fn save_snapshot(path: &Path, snapshot: &UsageSnapshot) -> Result<()> {
    let parent = path.parent().context("Web cache has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let result = (|| -> Result<()> {
        let bytes =
            simd_json::serde::to_vec(&(env!("CARGO_PKG_VERSION"), cache_timezone(), snapshot))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

impl UsageState {
    fn persistent(path: PathBuf) -> Self {
        let snapshot = match read_snapshot(&path) {
            Ok(snapshot) => Some(CachedUsage {
                snapshot: Arc::new(snapshot),
                completed_at: Instant::now(),
                from_disk: true,
            }),
            Err(error) => {
                if path.exists() {
                    eprintln!("Failed to read web cache {}: {error:#}", path.display());
                }
                None
            }
        };
        Self {
            cached: parking_lot::RwLock::new(snapshot),
            cache_path: Some(path),
            ..Default::default()
        }
    }

    async fn load(
        self: Arc<Self>,
        refresh: bool,
        startup: bool,
        loader: impl FnOnce() -> Result<UsageSnapshot> + Send + 'static,
    ) -> Result<Arc<UsageSnapshot>> {
        let requested_at = Instant::now();
        if !refresh
            && !startup
            && let Some(cached) = self.cached.read().as_ref()
        {
            return Ok(cached.snapshot.clone());
        }
        // A disconnected client must not release a still-running scan.
        tokio::spawn(async move {
            let _scan = self.scan.lock().await;
            if let Some(cached) = self.cached.read().as_ref()
                && (cached.completed_at >= requested_at
                    || (!refresh && (!startup || !cached.from_disk)))
            {
                return Ok(cached.snapshot.clone());
            }
            let cache_path = self.cache_path.clone();
            let snapshot = Arc::new(
                tokio::task::spawn_blocking(move || {
                    let snapshot = loader()?;
                    if let Some(path) = cache_path
                        && let Err(error) = save_snapshot(&path, &snapshot)
                    {
                        eprintln!("Failed to save web cache {}: {error:#}", path.display());
                    }
                    Ok::<_, anyhow::Error>(snapshot)
                })
                .await
                .context("Web usage scan failed")??,
            );
            *self.cached.write() = Some(CachedUsage {
                snapshot: snapshot.clone(),
                completed_at: Instant::now(),
                from_disk: false,
            });
            Ok(snapshot)
        })
        .await
        .context("Web usage task failed")?
    }
}

#[derive(Default, Deserialize)]
struct UsageQuery {
    #[serde(default)]
    refresh: bool,
    #[serde(default)]
    startup: bool,
}

fn load_usage() -> Result<UsageSnapshot> {
    create_analyzer_registry()
        .load_all_stats_parallel_scoped()
        .map(|stats| usage_snapshot(&stats))
}

async fn usage(State(state): State<Arc<UsageState>>, Query(query): Query<UsageQuery>) -> Response {
    let response = match state
        .clone()
        .load(query.refresh, query.startup, load_usage)
        .await
    {
        Ok(snapshot) => {
            let stale = state.cached.read().as_ref().is_some_and(|cached| {
                cached.from_disk || !Arc::ptr_eq(&cached.snapshot, &snapshot)
            });
            (
                [("x-splitrail-stale", if stale { "true" } else { "false" })],
                Json(UsageResponse {
                    snapshot: snapshot.as_ref(),
                    model_aliases: &state.model_aliases,
                }),
            )
                .into_response()
        }
        Err(error) => {
            eprintln!("Failed to load web usage: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Failed to load usage data",
            )
                .into_response()
        }
    };
    ([(header::CACHE_CONTROL, "no-store")], response).into_response()
}

async fn local_host_only(request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if host != "127.0.0.1"
        && !host.strip_prefix("127.0.0.1:").is_some_and(|port| {
            !port.is_empty()
                && port.bytes().all(|byte| byte.is_ascii_digit())
                && port.parse::<u16>().is_ok()
        })
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}

fn app_with_state(state: Arc<UsageState>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("web/index.html")) }))
        .route(
            "/icon.svg",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "image/svg+xml")],
                    include_str!("web/icon.svg"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("web/app.js"),
                )
            }),
        )
        .route(
            "/data.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("web/data.mjs"),
                )
            }),
        )
        .route(
            "/style.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("web/style.css"),
                )
            }),
        )
        .route("/api/usage", get(usage))
        .route(
            "/locales.mjs",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("web/locales.mjs"),
                )
            }),
        )
        .layer(from_fn(local_host_only))
        .with_state(state)
}

#[cfg(test)]
fn app() -> Router {
    app_with_state(Arc::new(UsageState::default()))
}

pub async fn run(port: u16, model_aliases: HashMap<String, String>) -> Result<()> {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("Failed to listen on {address}"))?;
    println!("Splitrail web: http://{address}");
    let mut state = dirs::cache_dir().map_or_else(UsageState::default, |directory| {
        UsageState::persistent(directory.join("splitrail").join("web-usage-v1.json"))
    });
    state.model_aliases = model_aliases;
    let state = Arc::new(state);
    if state.cached.read().is_some() {
        let refresh = state.clone();
        tokio::spawn(async move {
            if let Err(error) = refresh.load(false, true, load_usage).await {
                eprintln!("Failed to refresh startup web usage: {error:#}");
            }
        });
    }
    axum::serve(listener, app_with_state(state))
        .await
        .context("Web server stopped")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgenticCodingToolStats, Application, ConversationMessage, Stats};
    use chrono::{TimeZone, Utc};

    fn snapshot_with_input(input_tokens: u64) -> UsageSnapshot {
        UsageSnapshot {
            rows: vec![HourlyUsage {
                input_tokens,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn disk_snapshot_survives_restart_and_startup_refreshes_once() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.json");
        let state = Arc::new(UsageState::persistent(path.clone()));
        state
            .clone()
            .load(false, false, || Ok(snapshot_with_input(10)))
            .await
            .unwrap();
        let restarted = Arc::new(UsageState::persistent(path.clone()));
        assert!(restarted.cached.read().as_ref().unwrap().from_disk);
        let cached = restarted
            .clone()
            .load(false, false, || anyhow::bail!("must use disk"))
            .await
            .unwrap();
        assert_eq!(cached.rows[0].input_tokens, 10);
        restarted
            .clone()
            .load(false, true, || Ok(snapshot_with_input(20)))
            .await
            .unwrap();
        restarted
            .clone()
            .load(false, true, || anyhow::bail!("startup already refreshed"))
            .await
            .unwrap();
        assert_eq!(read_snapshot(&path).unwrap().rows[0].input_tokens, 20);
        let error = restarted
            .clone()
            .load(true, false, || anyhow::bail!("scan failed"))
            .await;
        assert!(error.is_err());
        assert_eq!(read_snapshot(&path).unwrap().rows[0].input_tokens, 20);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_cache_is_ignored_and_failed_write_preserves_previous_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.json");
        std::fs::write(&path, b"broken").unwrap();
        assert!(UsageState::persistent(path.clone()).cached.read().is_none());
        for (version, timezone) in [
            ("old".to_owned(), cache_timezone()),
            (env!("CARGO_PKG_VERSION").to_owned(), "different".to_owned()),
        ] {
            std::fs::write(
                &path,
                simd_json::serde::to_vec(&(version, timezone, snapshot_with_input(10))).unwrap(),
            )
            .unwrap();
            assert!(UsageState::persistent(path.clone()).cached.read().is_none());
        }
        // A directory cannot replace the previous cache file.
        let destination = directory.path().join("destination");
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(destination.join("previous"), b"keep").unwrap();
        assert!(save_snapshot(&destination, &snapshot_with_input(20)).is_err());
        assert_eq!(
            std::fs::read(destination.join("previous")).unwrap(),
            b"keep"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    }

    #[tokio::test]
    async fn cached_page_stays_available_while_startup_refresh_runs() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.json");
        save_snapshot(&path, &snapshot_with_input(10)).unwrap();
        let state = Arc::new(UsageState::persistent(path));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let refresh = tokio::spawn(state.clone().load(false, true, move || {
            started_tx.send(()).unwrap();
            finish_rx.blocking_recv().unwrap();
            Ok(snapshot_with_input(20))
        }));
        started_rx.await.unwrap();
        let cached = state
            .clone()
            .load(false, false, || anyhow::bail!("must not scan"))
            .await
            .unwrap();
        assert_eq!(cached.rows[0].input_tokens, 10);
        let waiting = state
            .clone()
            .load(false, true, || anyhow::bail!("must share startup scan"));
        tokio::pin!(waiting);
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        finish_tx.send(()).unwrap();
        assert_eq!(waiting.await.unwrap().rows[0].input_tokens, 20);
        refresh.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn reuses_snapshot_and_refreshes_explicitly() {
        let state = Arc::new(UsageState::default());
        let first = state
            .clone()
            .load(false, false, || Ok(snapshot_with_input(10)))
            .await
            .unwrap();
        let cached = state
            .clone()
            .load(false, false, || {
                anyhow::bail!("cached request must not scan")
            })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&first, &cached));
        assert_eq!(cached.rows[0].input_tokens, 10);
        let refreshed = state
            .clone()
            .load(true, false, || Ok(snapshot_with_input(20)))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &refreshed));
        assert_eq!(refreshed.rows[0].input_tokens, 20);
    }

    #[tokio::test]
    async fn concurrent_refreshes_share_one_scan() {
        let state = Arc::new(UsageState::default());
        state
            .clone()
            .load(false, false, || Ok(UsageSnapshot::default()))
            .await
            .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let first = tokio::spawn(state.clone().load(true, false, move || {
            started_tx.send(()).unwrap();
            finish_rx.blocking_recv().unwrap();
            Ok(UsageSnapshot::default())
        }));
        started_rx.await.unwrap();
        let second = state.clone().load(true, false, || {
            anyhow::bail!("concurrent request must not scan")
        });
        tokio::pin!(second);
        assert!(futures::poll!(second.as_mut()).is_pending());
        finish_tx.send(()).unwrap();
        let first = first.await.unwrap().unwrap();
        let second = second.await.unwrap();
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[tokio::test]
    async fn disconnected_client_does_not_release_running_scan() {
        let state = Arc::new(UsageState::default());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let request = tokio::spawn(state.clone().load(false, false, move || {
            started_tx.send(()).unwrap();
            finish_rx.blocking_recv().unwrap();
            Ok(UsageSnapshot::default())
        }));
        started_rx.await.unwrap();
        request.abort();
        assert!(request.await.err().unwrap().is_cancelled());
        let next = state.clone().load(false, false, || {
            anyhow::bail!("running scan must be reused")
        });
        tokio::pin!(next);
        assert!(futures::poll!(next.as_mut()).is_pending());
        finish_tx.send(()).unwrap();
        assert!(next.await.unwrap().rows.is_empty());
    }

    #[tokio::test]
    async fn failed_refresh_keeps_previous_snapshot_and_can_retry() {
        let state = Arc::new(UsageState::default());
        let first = state
            .clone()
            .load(false, false, || Ok(UsageSnapshot::default()))
            .await
            .unwrap();
        let error = state
            .clone()
            .load(true, false, || anyhow::bail!("scan failed"))
            .await
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "scan failed");
        let cached = state
            .clone()
            .load(false, false, || {
                anyhow::bail!("must preserve previous data")
            })
            .await
            .unwrap();
        assert!(Arc::ptr_eq(&first, &cached));
        let retry = state
            .clone()
            .load(true, false, || Ok(UsageSnapshot::default()))
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&first, &retry));
    }

    #[tokio::test]
    async fn serves_current_aliases_with_raw_cached_models() {
        use crate::reqwest_simd_json::ResponseSimdJsonExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let mut snapshot = snapshot_with_input(10);
        snapshot.rows[0].model = "group/auto-flash".into();
        let state = Arc::new(UsageState {
            model_aliases: HashMap::from([("group/auto-flash".into(), "flash".into())]),
            cached: parking_lot::RwLock::new(Some(CachedUsage {
                snapshot: Arc::new(snapshot),
                completed_at: Instant::now(),
                from_disk: true,
            })),
            ..Default::default()
        });
        let server =
            tokio::spawn(async move { axum::serve(listener, app_with_state(state)).await });
        let response: simd_json::OwnedValue = reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{address}/api/usage"))
            .send()
            .await
            .unwrap()
            .simd_json()
            .await
            .unwrap();
        assert_eq!(response["modelAliases"]["group/auto-flash"], "flash");
        assert_eq!(response["rows"][0]["model"], "group/auto-flash");
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn rejects_remote_hosts_and_serves_embedded_modules() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app()).await });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for host in [
            "example.com",
            "127.0.0.1.example.com",
            "localhost.example.com",
            "localhost",
            "127.0.0.1:garbage",
            "127.0.0.1:",
            "127.0.0.1:65536",
            "127.0.0.1:80@evil.com",
            "127.0.0.1:+80",
        ] {
            let response = client
                .get(format!("http://{address}/api/usage"))
                .header(header::HOST, host)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        for path in [
            "/",
            "/app.js",
            "/data.mjs",
            "/locales.mjs",
            "/style.css",
            "/icon.svg",
        ] {
            let response = client
                .get(format!("http://{address}{path}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert!(!response.text().await.unwrap().is_empty());
        }
        for host in ["127.0.0.1", "127.0.0.1:8765"] {
            let response = client
                .get(format!("http://{address}/"))
                .header(header::HOST, host)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        let response = client
            .post(format!("http://{address}/api/usage"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        server.abort();
        assert!(server.await.unwrap_err().is_cancelled());
    }

    #[test]
    fn groups_usage_by_local_hour_and_session_into_merged_projects() {
        let project: PathBuf = Path::new("/work/project").components().collect();
        let project_id = project.to_string_lossy().into_owned();
        let worktree_path = project
            .join(".worktrees")
            .join("feature")
            .to_string_lossy()
            .into_owned();
        let message = ConversationMessage {
            application: Application::CodexCli,
            date: Utc.with_ymd_and_hms(2026, 9, 22, 12, 0, 0).unwrap(),
            project_hash: "hash".into(),
            project_path: Some(project_id.clone()),
            conversation_hash: "session".into(),
            local_hash: None,
            global_hash: "message".into(),
            model: Some("gpt-test".into()),
            stats: Stats {
                input_tokens: 10,
                output_tokens: 5,
                cached_tokens: 4,
                reasoning_tokens: 2,
                cost: 0.01,
                tool_calls: 3,
                ..Default::default()
            },
            role: MessageRole::Assistant,
            uuid: None,
            session_name: None,
        };
        let mut other_model = message.clone();
        other_model.model = Some("other".into());
        other_model.global_hash = "other-message".into();
        let mut next_hour = message.clone();
        next_hour.date += chrono::Duration::hours(1);
        next_hour.global_hash = "next-hour".into();
        let mut user = message.clone();
        user.role = MessageRole::User;
        user.date -= chrono::Duration::minutes(30);
        user.session_name = Some("Fix login".into());
        let mut zero_usage = message.clone();
        zero_usage.project_hash = "unused".into();
        zero_usage.project_path = Some("/work/unused".into());
        zero_usage.stats = Stats::default();
        let mut worktree = message.clone();
        worktree.project_hash = String::new();
        worktree.project_path = Some(worktree_path.clone());
        worktree.conversation_hash = "worktree-session".into();
        let stats = MultiAnalyzerStats {
            analyzer_stats: vec![AgenticCodingToolStats {
                daily_stats: BTreeMap::new(),
                num_conversations: 2,
                messages: vec![
                    message.clone(),
                    message.clone(),
                    next_hour.clone(),
                    other_model,
                    user.clone(),
                    zero_usage,
                    worktree,
                ],
                analyzer_name: "Codex CLI".into(),
            }],
        };
        let snapshot = usage_snapshot(&stats);
        assert_eq!(snapshot.rows.len(), 4);
        let hour = message.date.with_timezone(&Local).hour() as u8;
        let row = snapshot
            .rows
            .iter()
            .find(|row| {
                row.model == "gpt-test" && row.hour == hour && row.session == "Codex CLI:session"
            })
            .unwrap();
        assert_eq!(row.project, project_id);
        assert_eq!(row.input_tokens, 20);
        assert_eq!(row.output_tokens, 10);
        assert_eq!(row.cached_tokens, 8);
        assert_eq!(row.reasoning_tokens, 4);
        assert_eq!(row.messages, 2);
        assert_eq!(row.tool_calls, 6);
        assert!(snapshot.rows.iter().all(|row| row.project == project_id));

        // Sessions span user messages too, and a worktree session joins its repository.
        assert_eq!(snapshot.sessions.len(), 2);
        let session = &snapshot.sessions[0];
        assert_eq!(session.id, "Codex CLI:session");
        assert_eq!(session.name.as_deref(), Some("Fix login"));
        assert_eq!(session.start, user.date);
        assert_eq!(session.end, next_hour.date);
        assert_eq!(snapshot.sessions[1].project, project_id);
        assert_eq!(snapshot.projects.len(), 1);
        assert_eq!(snapshot.projects[0].paths, [project_id, worktree_path]);

        let mut other_tool = stats.analyzer_stats[0].clone();
        other_tool.analyzer_name = "Other tool".into();
        for message in &mut other_tool.messages {
            message.project_path = Some("/work/other".into());
            message.project_hash = "other-hash".into();
            message.session_name = Some("Other session".into());
        }
        let mut multi_tool = stats;
        multi_tool.analyzer_stats.push(other_tool);
        let snapshot = usage_snapshot(&multi_tool);
        assert_eq!(snapshot.sessions.len(), 4);
        let other = snapshot
            .sessions
            .iter()
            .find(|session| session.id == "Other tool:session")
            .unwrap();
        assert_eq!(other.tool, "Other tool");
        assert_eq!(other.project, "/work/other");
        assert_eq!(other.name.as_deref(), Some("Other session"));
        assert!(
            snapshot
                .rows
                .iter()
                .filter(|row| row.tool == "Other tool")
                .all(|row| row.session.starts_with("Other tool:") && row.project == "/work/other")
        );

        let mut unknown = message;
        unknown.project_path = None;
        unknown.model = None;
        unknown.stats = Stats {
            cost: 0.5,
            ..Default::default()
        };
        let snapshot = usage_snapshot(&MultiAnalyzerStats {
            analyzer_stats: vec![AgenticCodingToolStats {
                daily_stats: BTreeMap::new(),
                num_conversations: 1,
                messages: vec![unknown],
                analyzer_name: "Codex CLI".into(),
            }],
        });
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.rows[0].project, "hash");
        assert_eq!(snapshot.rows[0].model, "Unknown model");
        assert_eq!(snapshot.rows[0].input_tokens, 0);
        assert_eq!(snapshot.rows[0].cost, 0.5);
        assert_eq!(snapshot.sessions.len(), 1);
    }

    #[test]
    fn session_activity_counts_short_gaps_and_file_operations() {
        let message_at = |minute| ConversationMessage {
            application: Application::CodexCli,
            date: Utc.with_ymd_and_hms(2026, 9, 22, 12, minute, 0).unwrap(),
            project_hash: "hash".into(),
            project_path: Some("/work/project".into()),
            conversation_hash: "session".into(),
            local_hash: None,
            global_hash: format!("message-{minute}"),
            model: Some("gpt-test".into()),
            stats: Stats {
                input_tokens: 10,
                files_read: 2,
                files_edited: 1,
                files_added: 1,
                terminal_commands: 1,
                tool_calls: 1,
                ..Default::default()
            },
            role: MessageRole::Assistant,
            uuid: None,
            session_name: None,
        };
        let stats = MultiAnalyzerStats {
            analyzer_stats: vec![AgenticCodingToolStats {
                daily_stats: BTreeMap::new(),
                num_conversations: 1,
                messages: vec![message_at(0), message_at(2), message_at(10)],
                analyzer_name: "Codex CLI".into(),
            }],
        };

        let snapshot = usage_snapshot(&stats);
        assert_eq!(snapshot.rows.len(), 1);
        assert_eq!(snapshot.rows[0].files_read, 6);
        assert_eq!(snapshot.rows[0].files_edited, 3);
        assert_eq!(snapshot.rows[0].files_added, 3);
        assert_eq!(snapshot.rows[0].terminal_commands, 3);
        assert_eq!(snapshot.sessions[0].active_minutes, 2);
    }
}
