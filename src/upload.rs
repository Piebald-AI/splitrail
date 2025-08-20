use crate::config::Config;
use crate::reqwest_simd_json::{ReqwestSimdJsonExt, ResponseSimdJsonExt};
use crate::tui::UploadStatus;
use crate::types::{ConversationMessage, ErrorResponse, MultiAnalyzerStats, UploadResponse};
use crate::utils;
use anyhow::{Context, Result};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

static HTTP_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

pub async fn upload_message_stats<F>(
    messages: &[ConversationMessage],
    config: &mut Config,
    mut progress_callback: F,
) -> Result<()>
where
    F: FnMut(usize, usize),
{
    const CHUNK_SIZE: usize = 4500;
    if messages.is_empty() {
        return Ok(());
    }

    let client = HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .danger_accept_invalid_certs(true)
            .build()
            .expect("Failed to create HTTP client")
    });

    let chunks: Vec<_> = messages.chunks(CHUNK_SIZE).collect();
    let total_messages = messages.len();
    let mut messages_processed = 0;

    for chunk in chunks {
        // For smooth counting, we calculate the current message position based on chunk progress
        let messages_in_chunk = chunk.len();
        let chunk_start = messages_processed;

        // Run fast counter with non-blocking HTTP request
        let mut current_count = chunk_start;
        let target_count = chunk_start + messages_in_chunk;

        // Start the HTTP request
        let mut http_request = Box::pin(
            client
                .post(format!("{}/api/upload-stats", config.server.url))
                .header(
                    "Authorization",
                    format!("Bearer {}", config.server.api_token),
                )
                .header("Content-Type", "application/json")
                .simd_json(&chunk)
                .send(),
        );

        // Counter animation loop
        loop {
            tokio::select! {
                // HTTP request completed
                response = &mut http_request => {
                    let response = response?;

                    // Process response
                    if response.status().is_success() {
                        let upload_response: UploadResponse =
                            response.simd_json().await.context("Failed to parse response")?;

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
        if let Some(status) = status {
            if let Ok(mut s) = status.lock() {
                *s = value;
            }
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
                if let Some(ref status) = upload_status {
                    if let Ok(mut s) = status.lock() {
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
