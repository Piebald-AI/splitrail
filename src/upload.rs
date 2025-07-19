use crate::config::Config;
use crate::types::{ConversationMessage, FileOperationStats, UploadResponse, WebappStats};
use anyhow::{Context, Result};
use std::time::Duration;

fn parse_json_error(error_body: &str) -> Option<String> {
    // Try to parse JSON and extract error message from the defined API format
    if error_body.trim().starts_with('{') {
        if let Ok(json_value) = serde_json::from_str::<serde_json::Value>(error_body) {
            // The API returns errors in the format: {"error": "message"}
            if let Some(error_msg) = json_value.get("error") {
                if let Some(msg_str) = error_msg.as_str() {
                    return Some(msg_str.to_string());
                }
            }
        }
    }
    None
}

pub async fn upload_message_stats(
    messages: &Vec<ConversationMessage>,
    config: &mut Config,
) -> Result<()> {
    let date = chrono::Utc::now().timestamp_millis();
    config.set_last_date_uploaded(date.try_into().unwrap());
    config.save(true)?;

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.upload.timeout_seconds))
        .build()
        .context("Failed to create HTTP client")?;

    let response = client
        .post(format!("{}/api/upload-stats", config.server.url))
        .header(
            "Authorization",
            format!("Bearer {}", config.server.api_token),
        )
        .header("Content-Type", "application/json")
        .json(
            &messages
                .iter()
                .map(|m| WebappStats {
                    hash: "".to_string(),
                    message: m.clone(),
                })
                .collect::<Vec<WebappStats>>(),
        )
        .send()
        .await;

    match response {
        Ok(resp) => {
            if resp.status().is_success() {
                let upload_response: UploadResponse =
                    resp.json().await.context("Failed to parse response")?;

                if upload_response.success {
                    return Ok(());
                } else {
                    anyhow::bail!(
                        "Server returned error: {}",
                        upload_response
                            .error
                            .unwrap_or_else(|| "Unknown error".to_string())
                    );
                }
            } else {
                let status = resp.status();
                let error_text = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());

                // Parse JSON error if present
                let parsed_error = parse_json_error(&error_text);
                
                let error_message = match status.as_u16() {
                    400 => {
                        if let Some(json_msg) = parsed_error {
                            format!("Bad request: {}", json_msg)
                        } else if error_text.contains("invalid") || error_text.contains("validation") {
                            format!("Invalid data: {}", error_text)
                        } else if error_text.contains("missing") {
                            format!("Missing data: {}", error_text)
                        } else if error_text.contains("malformed") || error_text.contains("format") {
                            format!("Bad format: {}", error_text)
                        } else if !error_text.is_empty() && error_text != "Unknown error" && !error_text.starts_with('{') {
                            format!("Bad request: {}", error_text)
                        } else {
                            "Bad request (malformed JSON)".to_string()
                        }
                    },
                    401 => "Unauthorized".to_string(),
                    403 => "Invalid API token".to_string(),
                    404 => "Server not found".to_string(),
                    409 => "Conflict".to_string(),
                    422 => {
                        if let Some(json_msg) = parsed_error {
                            format!("Validation: {}", json_msg)
                        } else if !error_text.is_empty() && error_text != "Unknown error" {
                            format!("Validation: {}", error_text)
                        } else {
                            "Validation error".to_string()
                        }
                    },
                    429 => "Rate limited".to_string(),
                    500 => "Server error".to_string(),
                    502 => "Bad gateway".to_string(),
                    503 => "Service unavailable".to_string(),
                    504 => "Gateway timeout".to_string(),
                    _ => {
                        if let Some(json_msg) = parsed_error {
                            json_msg
                        } else if !error_text.is_empty() && error_text != "Unknown error" {
                            error_text.clone()
                        } else {
                            format!("Error {}", status.as_u16())
                        }
                    }
                };

                anyhow::bail!("{}", error_message);
            }
        }
        Err(e) => {
            return Err(e.into());
        }
    }
}

pub fn estimate_lines_added(file_ops: &FileOperationStats) -> u64 {
    // Estimate that edited files are mostly new content
    file_ops.lines_edited + (file_ops.lines_edited / 3)
}

pub fn estimate_lines_deleted(file_ops: &FileOperationStats) -> u64 {
    // Estimate that some edited content was deleted
    file_ops.lines_edited / 4
}

fn determine_language_from_files(_count: &u32) -> Option<String> {
    // This is a simplified implementation
    // In a real implementation, we'd track file extensions and map them to languages
    Some("typescript".to_string())
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
