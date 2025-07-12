use crate::config::Config;
use crate::types::{DailyStats, FileOperationStats};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::time::Duration;

#[derive(Debug, Serialize)]
struct UploadStatsRequest {
    date: String,
    stats: WebappStats,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct WebappStats {
    cost: f64,
    input_tokens: u64,
    output_tokens: u64,
    cached_tokens: u64,
    user_messages: u32,
    ai_messages: u32,
    tool_calls: u32,
    conversations: u32,
    max_flow_length_seconds: u64,
    files_read: u32,
    files_edited: u32,
    files_written: u32,
    lines_read: u64,
    lines_added: u64,
    lines_deleted: u64,
    lines_modified: u64,
    bytes_read: u64,
    bytes_edited: u64,
    bytes_written: u64,
    bash_commands: u32,
    glob_searches: u32,
    grep_searches: u32,
    todos_created: u32,
    todos_completed: u32,
    todos_in_progress: u32,
    todo_reads: u32,
    todo_writes: u32,
    code_lines: u64,
    docs_lines: u64,
    data_lines: u64,
    projects_data: HashMap<String, ProjectData>,
    languages_data: HashMap<String, LanguageData>,
    models_data: HashMap<String, u32>,
}

#[derive(Debug, Serialize)]
struct ProjectData {
    percentage: f64,
    lines: u64,
}

#[derive(Debug, Serialize)]
struct LanguageData {
    lines: u64,
    files: u64,
}

#[derive(Debug, Deserialize)]
struct UploadResponse {
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

pub async fn upload_daily_stats(
    stats: &BTreeMap<String, DailyStats>,
    config: &Config,
) -> Result<()> {
    if !config.is_configured() {
        anyhow::bail!("Configuration not complete. Please set server URL and API token.");
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.upload.timeout_seconds))
        .build()
        .context("Failed to create HTTP client")?;

    let mut upload_count = 0;
    let mut error_count = 0;

    // Filter stats based on upload configuration
    let stats_to_upload: Vec<_> = if config.upload.upload_today_only {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        stats
            .iter()
            .filter(|(date, _)| date.as_str() == today)
            .collect()
    } else {
        stats.iter().collect()
    };

    if stats_to_upload.is_empty() {
        println!("📡 No stats to upload");
        return Ok(());
    }

    println!(
        "📡 Uploading {} day(s) of stats to {}...",
        stats_to_upload.len(),
        config.server.url
    );

    for (date, daily_stats) in stats_to_upload {
        match upload_single_day(date, daily_stats, config, &client).await {
            Ok(_) => {
                upload_count += 1;
                println!("  ✅ Uploaded {}", date);
            }
            Err(e) => {
                error_count += 1;
                eprintln!("  ❌ Failed to upload {}: {}", date, e);
            }
        }
    }

    if upload_count > 0 {
        println!("🎉 Successfully uploaded {} day(s) of stats!", upload_count);
    }

    if error_count > 0 {
        println!("⚠️  {} upload(s) failed", error_count);
    }

    Ok(())
}

async fn upload_single_day(
    date: &str,
    stats: &DailyStats,
    config: &Config,
    client: &reqwest::Client,
) -> Result<()> {
    let webapp_stats = transform_stats_for_webapp(stats);
    let request = UploadStatsRequest {
        date: date.to_string(),
        stats: webapp_stats,
    };

    for attempt in 1..=config.upload.retry_attempts {
        let response = client
            .post("https://splitrail.dev/api/upload-stats")
            .header(
                "Authorization",
                format!("Bearer {}", config.server.api_token),
            )
            .header("Content-Type", "application/json")
            .json(&request)
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

                    if attempt == config.upload.retry_attempts {
                        anyhow::bail!("HTTP {} - {}", status, error_text);
                    } else {
                        eprintln!(
                            "    Attempt {}/{} failed: HTTP {} - retrying...",
                            attempt, config.upload.retry_attempts, status
                        );
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }
            Err(e) => {
                if attempt == config.upload.retry_attempts {
                    return Err(e.into());
                } else {
                    eprintln!(
                        "    Attempt {}/{} failed: {} - retrying...",
                        attempt, config.upload.retry_attempts, e
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    unreachable!()
}

fn transform_stats_for_webapp(stats: &DailyStats) -> WebappStats {
    // Extract project data from file operations
    let mut projects_data = HashMap::new();
    let mut languages_data = HashMap::new();

    // Analyze file operations to derive project and language data
    let total_lines = stats.file_operations.lines_read
        + stats.file_operations.lines_edited
        + stats.file_operations.lines_written;

    // For now, create a simple "main-project" entry
    // In a full implementation, we'd analyze file paths to determine actual projects
    if total_lines > 0 {
        projects_data.insert(
            "main-project".to_string(),
            ProjectData {
                percentage: 100.0,
                lines: total_lines,
            },
        );
    }

    // Extract languages from file type data
    for (file_type, count) in &stats.file_operations.file_types {
        let language = match file_type.as_str() {
            "source_code" => determine_language_from_files(count),
            _ => continue,
        };

        if let Some(lang) = language {
            languages_data.insert(
                lang.clone(),
                LanguageData {
                    lines: *count as u64,
                    files: 1, // Simplified - we don't track individual files
                },
            );
        }
    }

    // If no languages detected, add a default
    if languages_data.is_empty() && total_lines > 0 {
        languages_data.insert(
            "typescript".to_string(),
            LanguageData {
                lines: total_lines,
                files: 1,
            },
        );
    }

    // Calculate file type line counts from file_types
    let code_lines = stats
        .file_operations
        .file_types
        .get("source_code")
        .unwrap_or(&0)
        * 50; // Estimate lines per file
    let docs_lines = stats
        .file_operations
        .file_types
        .get("documentation")
        .unwrap_or(&0)
        * 30;
    let data_lines = stats.file_operations.file_types.get("data").unwrap_or(&0) * 20;

    // Convert BTreeMap to HashMap for models
    let models_data: HashMap<String, u32> =
        stats.models.iter().map(|(k, v)| (k.clone(), *v)).collect();

    WebappStats {
        cost: stats.cost,
        input_tokens: stats.input_tokens,
        output_tokens: stats.output_tokens,
        cached_tokens: stats.cached_tokens,
        user_messages: stats.user_messages,
        ai_messages: stats.ai_messages,
        tool_calls: stats.tool_calls,
        conversations: stats.conversations,
        max_flow_length_seconds: stats.max_flow_length_seconds,
        files_read: stats.file_operations.files_read,
        files_edited: stats.file_operations.files_edited,
        files_written: stats.file_operations.files_written,
        lines_read: stats.file_operations.lines_read,
        // For lines added/deleted/modified, we need to estimate from edit operations
        lines_added: estimate_lines_added(&stats.file_operations),
        lines_deleted: estimate_lines_deleted(&stats.file_operations),
        lines_modified: stats.file_operations.lines_edited,
        bytes_read: stats.file_operations.bytes_read,
        bytes_edited: stats.file_operations.bytes_edited,
        bytes_written: stats.file_operations.bytes_written,
        bash_commands: stats.file_operations.bash_commands,
        glob_searches: stats.file_operations.glob_searches,
        grep_searches: stats.file_operations.grep_searches,
        todos_created: stats.todo_stats.todos_created,
        todos_completed: stats.todo_stats.todos_completed,
        todos_in_progress: stats.todo_stats.todos_in_progress,
        todo_reads: stats.todo_stats.todo_reads,
        todo_writes: stats.todo_stats.todo_writes,
        code_lines: code_lines as u64,
        docs_lines: docs_lines as u64,
        data_lines: data_lines as u64,
        projects_data,
        languages_data,
        models_data,
    }
}

fn estimate_lines_added(file_ops: &FileOperationStats) -> u64 {
    // Estimate that written files are mostly new content
    file_ops.lines_written + (file_ops.lines_edited / 3)
}

fn estimate_lines_deleted(file_ops: &FileOperationStats) -> u64 {
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
