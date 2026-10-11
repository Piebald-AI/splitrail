use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use anyhow::{Context, Result};
use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::{Next, from_fn};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Local, Timelike, Utc};
use serde::Serialize;

use crate::create_analyzer_registry;
use crate::types::{MessageRole, MultiAnalyzerStats};

#[derive(Default, Serialize)]
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
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageSnapshot {
    scanned_at: DateTime<Utc>,
    rows: Vec<HourlyUsage>,
}

fn usage_snapshot(stats: &MultiAnalyzerStats) -> UsageSnapshot {
    type HourKey = (String, u8, String, String, String, String);
    let mut hours: BTreeMap<HourKey, HourlyUsage> = BTreeMap::new();
    for analyzer in &stats.analyzer_stats {
        for message in &analyzer.messages {
            let tool = analyzer.analyzer_name.as_str();
            let project = message
                .project_path
                .as_deref()
                .unwrap_or(&message.project_hash);
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
                    session: message.conversation_hash.clone(),
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
        }
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
    UsageSnapshot {
        scanned_at: Utc::now(),
        rows,
    }
}

fn load_usage() -> Result<UsageSnapshot> {
    create_analyzer_registry()
        .load_all_stats_parallel_scoped()
        .map(|stats| usage_snapshot(&stats))
}

async fn usage() -> Response {
    let result = tokio::task::spawn_blocking(load_usage)
        .await
        .context("Web usage scan task failed")
        .and_then(|result| result);
    let response = match result {
        Ok(snapshot) => Json(snapshot).into_response(),
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

fn app() -> Router {
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
        .layer(from_fn(local_host_only))
}

pub async fn run(port: u16) -> Result<()> {
    let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("Failed to listen on {address}"))?;
    println!("Splitrail web: http://{address}");
    axum::serve(listener, app())
        .await
        .context("Web server stopped")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgenticCodingToolStats, Application, ConversationMessage, Stats};
    use chrono::{TimeZone, Utc};

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
        for path in ["/", "/app.js", "/data.mjs", "/style.css", "/icon.svg"] {
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
    fn groups_assistant_usage_by_local_hour_model_project_and_session() {
        let message = ConversationMessage {
            application: Application::CodexCli,
            date: Utc.with_ymd_and_hms(2026, 9, 22, 12, 0, 0).unwrap(),
            project_hash: "hash".into(),
            project_path: Some("/work/project".into()),
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
        worktree.project_path = Some("/work/project/.worktrees/feature".into());
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
            .find(|row| row.model == "gpt-test" && row.hour == hour && row.session == "session")
            .unwrap();
        assert_eq!(row.project, "/work/project");
        assert_eq!(row.input_tokens, 20);
        assert_eq!(row.output_tokens, 10);
        assert_eq!(row.cached_tokens, 8);
        assert_eq!(row.reasoning_tokens, 4);
        assert_eq!(row.messages, 2);
        let worktree_row = snapshot
            .rows
            .iter()
            .find(|row| row.session == "worktree-session")
            .unwrap();
        assert_eq!(worktree_row.project, "/work/project/.worktrees/feature");
        assert_eq!(worktree_row.input_tokens, 10);
        assert!(
            snapshot
                .rows
                .iter()
                .all(|row| row.project != "/work/unused")
        );
        assert_eq!(
            row.date,
            message
                .date
                .with_timezone(&Local)
                .format("%Y-%m-%d")
                .to_string()
        );
        assert!(snapshot.scanned_at <= Utc::now());

        let mut unknown = message;
        unknown.project_path = None;
        unknown.model = None;
        let stats = MultiAnalyzerStats {
            analyzer_stats: vec![AgenticCodingToolStats {
                daily_stats: BTreeMap::new(),
                num_conversations: 1,
                messages: vec![unknown],
                analyzer_name: "Codex CLI".into(),
            }],
        };
        let snapshot = usage_snapshot(&stats);
        assert_eq!(snapshot.rows[0].project, "hash");
        assert_eq!(snapshot.rows[0].model, "Unknown model");
    }
}
