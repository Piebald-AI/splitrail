use super::*;
use crate::types::{
    AgenticCodingToolStats, Application, ConversationMessage, MessageRole, MultiAnalyzerStats,
    Stats,
};
use chrono::Utc;
use std::collections::BTreeMap;
use std::env;
use std::io::ErrorKind;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

static TEST_HOME_DIR: OnceLock<std::path::PathBuf> = OnceLock::new();

fn set_test_home() -> std::path::PathBuf {
    let path = TEST_HOME_DIR
        .get_or_init(|| {
            let dir = TempDir::new().expect("tempdir");
            dir.into_path()
        })
        .clone();
    // Setting environment variables is unsafe in Rust 2024.
    unsafe {
        env::set_var("HOME", &path);
    }
    path
}

fn make_test_message(conversation_hash: &str) -> ConversationMessage {
    ConversationMessage {
        application: Application::ClaudeCode,
        date: Utc::now(),
        project_hash: "project".to_string(),
        conversation_hash: conversation_hash.to_string(),
        local_hash: None,
        global_hash: format!("global-{conversation_hash}"),
        model: Some("test-model".to_string()),
        stats: Stats::default(),
        role: MessageRole::User,
        uuid: None,
        session_name: None,
    }
}

fn make_stats_with_messages(messages: Vec<ConversationMessage>) -> MultiAnalyzerStats {
    MultiAnalyzerStats {
        analyzer_stats: vec![AgenticCodingToolStats {
            daily_stats: BTreeMap::new(),
            num_conversations: 1,
            messages,
            analyzer_name: "test-analyzer".to_string(),
        }],
    }
}

async fn start_test_server(
    status_line: &str,
    body: &str,
    expected_requests: usize,
    request_counter: Arc<AtomicUsize>,
) -> Option<String> {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(e) if e.kind() == ErrorKind::PermissionDenied => {
            // In restricted environments (like some CI or sandboxes), binding to
            // a local port may not be permitted. In that case, skip the tests
            // that rely on an HTTP server by returning None and letting the
            // caller short-circuit.
            return None;
        }
        Err(e) => panic!("failed to bind test listener: {e}"),
    };

    let addr = listener.local_addr().expect("local_addr");
    let base_url = format!("http://{}", addr);
    let status_line = status_line.to_string();
    let body = body.to_string();

    tokio::spawn(async move {
        for _ in 0..expected_requests {
            let (mut socket, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(_) => return,
            };

            let mut buf = [0u8; 4096];
            let _ = socket.read(&mut buf).await;

            request_counter.fetch_add(1, Ordering::SeqCst);

            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {len}\r\nContent-Type: application/json\r\n\r\n{body}",
                status = status_line,
                len = body.len(),
                body = body,
            );

            let _ = socket.write_all(response.as_bytes()).await;
        }
    });

    Some(base_url)
}

#[tokio::test]
async fn upload_message_stats_empty_messages_returns_ok_and_no_progress() {
    let mut config = Config::default();
    let mut progress_calls = 0usize;

    upload_message_stats(&[], &mut config, |_, _| {
        progress_calls += 1;
    })
    .await
    .expect("upload should succeed");

    assert_eq!(progress_calls, 0);
    assert_eq!(config.upload.last_date_uploaded, 0);
}

#[tokio::test]
async fn upload_message_stats_success_updates_progress_and_config() {
    set_test_home();

    let request_counter = Arc::new(AtomicUsize::new(0));
    let base_url = match start_test_server(
        "200 OK",
        r#"{"success":true}"#,
        1,
        request_counter.clone(),
    )
    .await
    {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: unable to bind local HTTP server");
            return;
        }
    };

    let mut config = Config::default();
    config.server.url = base_url;
    config.server.api_token = "TEST_TOKEN".to_string();

    let messages = vec![make_test_message("c1")];
    let progress_values: Arc<Mutex<Vec<(usize, usize)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let progress_values_clone = progress_values.clone();

    upload_message_stats(&messages, &mut config, move |current, total| {
        let mut guard = progress_values_clone.lock().unwrap();
        guard.push((current, total));
    })
    .await
    .expect("upload should succeed");

    assert_eq!(request_counter.load(Ordering::SeqCst), 1);

    let recorded = progress_values.lock().unwrap();
    assert!(!recorded.is_empty(), "progress callback should be called");
    let (final_current, final_total) = recorded[recorded.len() - 1];
    assert_eq!(final_total, messages.len());
    assert_eq!(final_current, messages.len());

    assert!(
        config.upload.last_date_uploaded > 0,
        "last_date_uploaded should be updated"
    );

    let saved = Config::load()
        .expect("load config")
        .expect("config should exist on disk");
    assert_eq!(saved.upload.last_date_uploaded, config.upload.last_date_uploaded);
}

#[tokio::test]
async fn upload_message_stats_server_error_plain_text_propagates_message() {
    set_test_home();

    let request_counter = Arc::new(AtomicUsize::new(0));
    let base_url = match start_test_server(
        "500 Internal Server Error",
        "plain error message",
        1,
        request_counter.clone(),
    )
    .await
    {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: unable to bind local HTTP server");
            return;
        }
    };

    let mut config = Config::default();
    config.server.url = base_url;
    config.server.api_token = "TEST_TOKEN".to_string();

    let messages = vec![make_test_message("c1")];

    let err = upload_message_stats(&messages, &mut config, |_c, _t| {})
        .await
        .expect_err("upload should fail");

    assert_eq!(request_counter.load(Ordering::SeqCst), 1);
    let msg = format!("{err}");
    assert!(
        msg.contains("plain error message"),
        "error message should include server message, got: {msg}"
    );
}

#[tokio::test]
async fn upload_message_stats_server_error_json_uses_error_field() {
    set_test_home();

    let request_counter = Arc::new(AtomicUsize::new(0));
    let base_url = match start_test_server(
        "400 Bad Request",
        r#"{"error":"json error message"}"#,
        1,
        request_counter.clone(),
    )
    .await
    {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: unable to bind local HTTP server");
            return;
        }
    };

    let mut config = Config::default();
    config.server.url = base_url;
    config.server.api_token = "TEST_TOKEN".to_string();

    let messages = vec![make_test_message("c1")];

    let err = upload_message_stats(&messages, &mut config, |_c, _t| {})
        .await
        .expect_err("upload should fail");

    assert_eq!(request_counter.load(Ordering::SeqCst), 1);
    let msg = format!("{err}");
    assert!(
        msg.contains("json error message"),
        "error message should include JSON error field, got: {msg}"
    );
}

#[tokio::test]
async fn upload_message_stats_large_batch_is_split_into_chunks() {
    set_test_home();

    // Use more than 3000 messages to trigger multiple chunks.
    let message_count = 3005usize;

    let request_counter = Arc::new(AtomicUsize::new(0));
    // Expect two HTTP requests due to chunking.
    let base_url = match start_test_server(
        "200 OK",
        r#"{"success":true}"#,
        2,
        request_counter.clone(),
    )
    .await
    {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: unable to bind local HTTP server");
            return;
        }
    };

    let mut config = Config::default();
    config.server.url = base_url;
    config.server.api_token = "TEST_TOKEN".to_string();

    let mut messages = Vec::with_capacity(message_count);
    for i in 0..message_count {
        messages.push(make_test_message(&format!("c{i}")));
    }

    upload_message_stats(&messages, &mut config, |_c, _t| {})
        .await
        .expect("upload should succeed");

    assert_eq!(
        request_counter.load(Ordering::SeqCst),
        2,
        "expected two HTTP requests for chunked upload"
    );
}

#[tokio::test]
async fn perform_background_upload_no_config_keeps_status_unchanged() {
    let _home = set_test_home();

    // Ensure there is no config file.
    let config_path = Config::config_path().expect("config_path");
    if config_path.exists() {
        std::fs::remove_file(&config_path).expect("remove existing config");
    }

    let stats = MultiAnalyzerStats {
        analyzer_stats: Vec::new(),
    };

    let status = Arc::new(Mutex::new(UploadStatus::MissingConfig));
    let status_clone = status.clone();

    perform_background_upload(stats, Some(status_clone), None).await;

    let final_status = status.lock().unwrap().clone();
    assert!(
        matches!(final_status, UploadStatus::MissingConfig),
        "status should remain unchanged when config is missing"
    );
}

#[tokio::test]
async fn perform_background_upload_unconfigured_config_keeps_status_unchanged() {
    let _home = set_test_home();

    // Save a default config which is not configured (missing API token).
    let config = Config::default();
    config.save(true).expect("save default config");

    let stats = MultiAnalyzerStats {
        analyzer_stats: Vec::new(),
    };

    let status = Arc::new(Mutex::new(UploadStatus::MissingApiToken));
    let status_clone = status.clone();

    perform_background_upload(stats, Some(status_clone), None).await;

    let final_status = status.lock().unwrap().clone();
    assert!(
        matches!(final_status, UploadStatus::MissingApiToken),
        "status should remain unchanged when config is not fully configured"
    );
}

#[tokio::test]
async fn perform_background_upload_propagates_upload_errors_to_status() {
    let _home = set_test_home();

    let request_counter = Arc::new(AtomicUsize::new(0));
    let base_url = match start_test_server(
        "500 Internal Server Error",
        "background upload error",
        1,
        request_counter.clone(),
    )
    .await
    {
        Some(url) => url,
        None => {
            eprintln!("Skipping test: unable to bind local HTTP server");
            return;
        }
    };

    let mut config = Config::default();
    config.server.url = base_url;
    config.server.api_token = "TEST_TOKEN".to_string();
    config
        .save(true)
        .expect("save configured config for background upload");

    let messages = vec![make_test_message("c1")];
    let stats = make_stats_with_messages(messages);

    let status = Arc::new(Mutex::new(UploadStatus::None));
    let status_clone = status.clone();

    perform_background_upload(stats, Some(status_clone), None).await;

    assert_eq!(request_counter.load(Ordering::SeqCst), 1);

    let final_status = status.lock().unwrap().clone();
    match final_status {
        UploadStatus::Failed(msg) => {
            assert!(
                msg.contains("background upload error"),
                "status error message should include server message, got: {msg}"
            );
        }
        other => panic!("expected Failed status, got: {:?}", other),
    }
}
