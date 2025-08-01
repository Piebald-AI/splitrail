use crate::config::Config;
use crate::types::{ConversationMessage, ErrorResponse, UploadResponse};
use anyhow::{Context, Result};
use std::sync::OnceLock;
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
    const CHUNK_SIZE: usize = 6000;
    if messages.is_empty() {
        return Ok(());
    }

    let client = HTTP_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(config.upload.timeout_seconds))
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
                .json(&chunk)
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
                            response.json().await.context("Failed to parse response")?;

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

                        if let Ok(error_res) = serde_json::from_str::<ErrorResponse>(&error_text) {
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
                        let jump_size = ((target_count - current_count) / 50).max(1).min(100);
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

pub fn show_upload_help() {
    println!();
    println!("To enable automatic uploads to the Splitrail Leaderboard:");
    println!("  1. Get your API token from the leaderboard webapp");
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
