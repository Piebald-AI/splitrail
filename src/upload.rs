use crate::config::Config;
use crate::reqwest_simd_json::{ReqwestSimdJsonExt, ResponseSimdJsonExt};
use crate::tui::UploadStatus;
use crate::types::{ConversationMessage, ErrorResponse, MultiAnalyzerStats, UploadResponse};
use crate::utils;
use anyhow::{Context, Result};
use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

fn upload_log_path() -> PathBuf {
    std::env::temp_dir().join("SPLITRAIL.log")
}

fn append_upload_log(line: &str) {
    use std::io::Write;

    let log_path = upload_log_path();
    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(f) => f,
        Err(_) => return,
    };

    let _ = writeln!(file, "{line}");
}

fn upload_debug_enabled() -> bool {
    std::env::var("SPLITRAIL_UPLOAD_DEBUG")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn upload_debug_log(line: impl Into<String>) {
    let line = line.into();
    eprintln!("{line}");
    append_upload_log(&line);
}

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

/// Get the shared HTTP client singleton
pub fn get_http_client() -> &'static reqwest::Client {
    HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .danger_accept_invalid_certs(true)
            .build()
            .expect("Failed to create HTTP client")
    })
}

#[cfg(test)]
mod tests;

pub async fn upload_message_stats<F>(
    messages: &[ConversationMessage],
    config: &mut Config,
    mut progress_callback: F,
) -> Result<()>
where
    F: FnMut(usize, usize),
{
    const CHUNK_SIZE: usize = 3000;
    if messages.is_empty() {
        return Ok(());
    }

    let upload_debug = upload_debug_enabled();

    if upload_debug {
        // Printed once per run, and early, so users see it even if the TUI is busy.
        let header1 = "[splitrail upload] debug enabled (SPLITRAIL_UPLOAD_DEBUG=1)";
        let header2 = format!(
            "[splitrail upload] chunk_size={CHUNK_SIZE} server={} retry_attempts={}",
            config.server.url, config.upload.retry_attempts
        );
        let header3 = "[splitrail upload] Legend: prep_ms=serialize_json wait_ms=server+network parse_ms=decode_response";
        let log_path_display = upload_log_path();
        let header4 = format!(
            "[splitrail upload] writing logs to {}",
            log_path_display.display()
        );

        utils::warn_once(header1.to_string());
        utils::warn_once(header2.clone());
        utils::warn_once(header3.to_string());
        utils::warn_once(header4.clone());

        append_upload_log(header1);
        append_upload_log(&header2);
        append_upload_log(header3);
        append_upload_log(&header4);
    }

    let client = get_http_client();

    let chunks: Vec<_> = messages.chunks(CHUNK_SIZE).collect();
    let total_messages = messages.len();
    let mut messages_processed = 0;

    for (chunk_index, chunk) in chunks.iter().enumerate() {
        // For smooth counting, we calculate the current message position based on chunk progress
        let messages_in_chunk = chunk.len();
        let chunk_start = messages_processed;

        if upload_debug {
            upload_debug_log(format!(
                "[splitrail upload] chunk {}/{} start: size={} processed_before={} (legend above)",
                chunk_index + 1,
                chunks.len(),
                messages_in_chunk,
                chunk_start
            ));
        }

        // Run fast counter with non-blocking HTTP request
        let mut current_count = chunk_start;
        let target_count = chunk_start + messages_in_chunk;

        // Start the HTTP request
        let timezone = utils::get_local_timezone();
        let prep_start = Instant::now();
        let mut http_request = Box::pin(
            client
                .post(format!("{}/api/upload-stats", config.server.url))
                .header(
                    "Authorization",
                    format!("Bearer {}", config.server.api_token),
                )
                .header("Content-Type", "application/json")
                .header("X-Timezone", &timezone)
                .simd_json(chunk)
                .send(),
        );
        let prep_ms = prep_start.elapsed().as_millis();
        let wait_start = Instant::now();

        // Counter animation loop
        loop {
            tokio::select! {
                // HTTP request completed
                response = &mut http_request => {
                    let response = response?;
                    let wait_ms = wait_start.elapsed().as_millis();

                    if upload_debug {
                        upload_debug_log(format!(
                            "[splitrail upload] chunk {}/{} response: status={} prep_ms={} wait_ms={} (see {})",
                            chunk_index + 1,
                            chunks.len(),
                            response.status(),
                            prep_ms,
                            wait_ms,
                            upload_log_path().display(),
                        ));
                    }

                    // Process response
                    if response.status().is_success() {
                        let parse_start = Instant::now();
                        let upload_response: UploadResponse =
                            response.simd_json().await.context("Failed to parse response")?;
                        let parse_ms = parse_start.elapsed().as_millis();

                        if upload_debug {
                            upload_debug_log(format!(
                                "[splitrail upload] chunk {}/{} parsed: success={} parse_ms={} (see {})",
                                chunk_index + 1,
                                chunks.len(),
                                upload_response.success,
                                parse_ms,
                                upload_log_path().display(),
                            ));
                        }

                        if !upload_response.success {
                            anyhow::bail!(
                                "Server returned error: {}",
                                upload_response
                                    .error
                                    .unwrap_or_else(|| "Unknown error".to_string())
                            );
                        }
                    } else {
                        let error_text = response
                            .text()
                            .await
                            .unwrap_or_else(|_| "Unknown error".to_string());

                        if let Ok(error_res) = simd_json::from_slice::<ErrorResponse>(&mut error_text.clone().into_bytes()) {
                            anyhow::bail!("{}", error_res.error);
                        }

                        anyhow::bail!("{}", error_text);
                    }

                    // Show final state and exit
                    progress_callback(target_count, total_messages);
                    break;
                }

                // Counter animation tick
                _ = tokio::time::sleep(Duration::from_micros(100)) => {
                    if current_count < target_count {
                        // Jump multiple messages at once for very fast counting
                        let jump_size = ((target_count - current_count) / 50).clamp(1, 100);
                        current_count = (current_count + jump_size).min(target_count);
                        progress_callback(current_count, total_messages);
                    } else {
                        // Reached target, show animated dots while waiting for HTTP
                        progress_callback(current_count, total_messages);
                        // Continue calling the callback to keep dots animating
                        // (TUI handles the actual dots animation timing)
                    }
                }
            }
        }

        messages_processed += messages_in_chunk;
    }

    let date = chrono::Utc::now().timestamp_millis();
    config.set_last_date_uploaded(date);
    config.save(true)?;

    Ok(())
}

/// Helper to set upload status atomically.
fn set_upload_status(status: &Option<Arc<Mutex<UploadStatus>>>, value: UploadStatus) {
    if let Some(status) = status {
        *status.lock() = value;
    }
}

/// Creates an upload progress callback that updates the TUI status.
fn make_progress_callback(
    upload_status: Option<Arc<Mutex<UploadStatus>>>,
) -> impl FnMut(usize, usize) {
    move |current, total| {
        if let Some(ref status) = upload_status {
            let mut s = status.lock();
            match &*s {
                UploadStatus::Uploading { dots, .. } => {
                    *s = UploadStatus::Uploading {
                        current,
                        total,
                        dots: *dots,
                    };
                }
                _ => {
                    *s = UploadStatus::Uploading {
                        current,
                        total,
                        dots: 0,
                    };
                }
            }
        }
    }
}

/// Handles the result of an upload operation, updating status accordingly.
async fn handle_upload_result(
    result: Option<anyhow::Result<()>>,
    upload_status: &Option<Arc<Mutex<UploadStatus>>>,
) {
    match result {
        Some(Ok(_)) => {
            set_upload_status(upload_status, UploadStatus::Uploaded);
            // Hide success message after 3 seconds
            tokio::time::sleep(Duration::from_secs(3)).await;
            set_upload_status(upload_status, UploadStatus::None);
        }
        Some(Err(e)) => {
            // Keep error messages visible permanently
            set_upload_status(upload_status, UploadStatus::Failed(format!("{e:#}")));
        }
        None => (), // Config not available or nothing to upload
    }
}

/// Upload pre-filtered messages directly (used for incremental uploads from watcher).
/// This is more efficient than loading all stats and filtering afterwards.
/// If `on_success` is provided, it will be called after a successful upload.
pub async fn perform_background_upload_messages<F>(
    messages: Vec<ConversationMessage>,
    upload_status: Option<Arc<Mutex<UploadStatus>>>,
    initial_delay_ms: Option<u64>,
    on_success: Option<F>,
) where
    F: FnOnce(),
{
    // Optional initial delay
    if let Some(delay) = initial_delay_ms {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    let upload_result = async {
        let mut config = Config::load().ok().flatten()?;
        if !config.is_configured() {
            return None;
        }

        if messages.is_empty() {
            return Some(Ok(())); // Nothing to upload
        }

        let result = upload_message_stats(
            &messages,
            &mut config,
            make_progress_callback(upload_status.clone()),
        )
        .await;

        // Call on_success callback if upload succeeded
        if result.is_ok()
            && let Some(callback) = on_success
        {
            callback();
        }

        Some(result)
    }
    .await;

    handle_upload_result(upload_result, &upload_status).await;
}

/// Upload stats from all analyzers (used for initial startup upload).
/// Filters messages by last upload timestamp before uploading.
pub async fn perform_background_upload(
    stats: MultiAnalyzerStats,
    upload_status: Option<Arc<Mutex<UploadStatus>>>,
    initial_delay_ms: Option<u64>,
) {
    // Optional initial delay
    if let Some(delay) = initial_delay_ms {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    // Load config and filter messages
    let messages = async {
        let config = Config::load().ok().flatten()?;
        if !config.is_configured() {
            return None;
        }

        let all_messages: Vec<_> = stats
            .analyzer_stats
            .into_iter()
            .flat_map(|s| s.messages)
            .collect();

        utils::get_messages_later_than(config.upload.last_date_uploaded, all_messages)
            .await
            .ok()
    }
    .await;

    if let Some(msgs) = messages {
        // Delegate to the messages-based upload (no additional delay, no callback)
        perform_background_upload_messages::<fn()>(msgs, upload_status, None, None).await;
    }
}

pub fn show_upload_help() {
    println!();
    println!("To enable automatic uploads to Splitrail Cloud:");
    println!("  1. Get your API token from https://splitrail.dev/settings");
    println!("  2. Configure splitrail:");
    println!("     splitrail config set api-token YOUR_TOKEN_HERE");
    println!("     splitrail config set auto-upload true");
    println!();
    println!("Manual upload:");
    println!("  splitrail upload");
    println!();
    println!("Check configuration:");
    println!("  splitrail config show");
}
