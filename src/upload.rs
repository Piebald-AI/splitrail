use crate::config::Config;
use crate::types::{
    ConversationMessage, ErrorResponse, FileOperationStats, UploadResponse, WebappStats,
};
use anyhow::{Context, Result};
use std::time::Duration;

pub async fn upload_message_stats(
    messages: &[ConversationMessage],
    config: &mut Config,
) -> Result<()> {
    let date = chrono::Utc::now().timestamp_millis();
    config.set_last_date_uploaded(date);
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
                    hash: match m {
                        ConversationMessage::AI { hash: Some(h), .. } => h.clone(),
                        ConversationMessage::User { hash: Some(h), .. } => h.clone(),
                        // Fallback for messages without hash (shouldn't happen with our updates)
                        _ => "missing_hash".to_string(),
                    },
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
                    Ok(())
                } else {
                    anyhow::bail!(
                        "Server returned error: {}",
                        upload_response
                            .error
                            .unwrap_or_else(|| "Unknown error".to_string())
                    );
                }
            } else {
                let error_text = resp
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string());

                if let Ok(error_res) = serde_json::from_str::<ErrorResponse>(&error_text) {
                    anyhow::bail!("{}", error_res.error);
                }

                anyhow::bail!("{}", error_text);
            }
        }
        Err(e) => Err(e.into()),
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
