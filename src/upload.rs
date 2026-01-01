use crate::config::Config;
use crate::reqwest_simd_json::{ReqwestSimdJsonExt, ResponseSimdJsonExt};
use crate::tui::UploadStatus;
use crate::types::{ConversationMessage, ErrorResponse, MultiAnalyzerStats, UploadResponse};
use crate::utils;
use anyhow::{Context, Result};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

fn upload_log_path() -> &'static str {
    "/tmp/SPLITRAIL.log"
}

fn append_upload_log(line: &str) {
    use std::io::Write;

    let mut file = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(upload_log_path())
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
        let header4 = format!("[splitrail upload] writing logs to {}", upload_log_path());

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
                            upload_log_path(),
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
                                upload_log_path(),
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

pub async fn perform_background_upload(
    stats: MultiAnalyzerStats,
    upload_status: Option<Arc<Mutex<UploadStatus>>>,
    initial_delay_ms: Option<u64>,
) {
    // Helper to set status
    fn set_status(status: &Option<Arc<Mutex<UploadStatus>>>, value: UploadStatus) {
        if let Some(status) = status
            && let Ok(mut s) = status.lock()
        {
            *s = value;
        }
    }

    // Optional initial delay
    if let Some(delay) = initial_delay_ms {
        tokio::time::sleep(Duration::from_millis(delay)).await;
    }

    let upload_result = async {
        let mut config = Config::load().ok().flatten()?;
        if !config.is_configured() {
            return None;
        }

        let mut messages = vec![];
        for analyzer_stats in stats.analyzer_stats {
            messages.extend(analyzer_stats.messages);
        }

        let messages = utils::get_messages_later_than(config.upload.last_date_uploaded, messages)
            .await
            .ok()?;

        if messages.is_empty() {
            return Some(Ok(())); // Nothing new to upload
        }

        Some(
            upload_message_stats(&messages, &mut config, |current, total| {
                // Update upload progress
                if let Some(ref status) = upload_status
                    && let Ok(mut s) = status.lock()
                {
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
            })
            .await,
        )
    }
    .await;

    match upload_result {
        Some(Ok(_)) => {
            set_status(&upload_status, UploadStatus::Uploaded);
            // Hide success message after 3 seconds
            tokio::time::sleep(Duration::from_secs(3)).await;
            set_status(&upload_status, UploadStatus::None);
        }
        Some(Err(e)) => {
            // Keep error messages visible permanently
            set_status(&upload_status, UploadStatus::Failed(format!("{e:#}")));
        }
        None => (), // Config not available or nothing to upload
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
