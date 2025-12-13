use anyhow::Result;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::reqwest_simd_json::ResponseSimdJsonExt;
use crate::upload::get_http_client;

const GITHUB_RELEASES_URL: &str =
    "https://api.github.com/repos/Piebald-AI/splitrail/releases/latest";
const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub enum UpdateStatus {
    Checking,
    Available { latest: String, current: String },
    UpToDate,
    CheckFailed,
    Dismissed,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

/// Parse version string (with optional 'v' prefix) into (major, minor, patch)
fn parse_version(version: &str) -> Option<(u32, u32, u32)> {
    let v = version.strip_prefix('v').unwrap_or(version);
    let parts: Vec<&str> = v.split('.').collect();
    if parts.len() >= 3 {
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    } else {
        None
    }
}

/// Returns true if `latest` is newer than `current`.
///
/// Uses Rust's lexicographic tuple comparison, which compares element by element
/// from left to right, stopping at the first unequal pair. This correctly implements
/// semantic versioning precedence: major > minor > patch.
///
/// Examples:
/// - (1, 1, 0) > (1, 0, 1) because minor 1 > 0 (patch is ignored)
/// - (2, 0, 0) > (1, 9, 9) because major 2 > 1 (minor/patch ignored)
fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Check GitHub for the latest release version
pub async fn check_for_updates() -> UpdateStatus {
    match fetch_latest_version().await {
        Ok(latest) => {
            if is_newer(&latest, CURRENT_VERSION) {
                UpdateStatus::Available {
                    latest: latest.strip_prefix('v').unwrap_or(&latest).to_string(),
                    current: CURRENT_VERSION.to_string(),
                }
            } else {
                UpdateStatus::UpToDate
            }
        }
        Err(_) => UpdateStatus::CheckFailed,
    }
}

async fn fetch_latest_version() -> Result<String> {
    let client = get_http_client();

    let response = client
        .get(GITHUB_RELEASES_URL)
        .header("User-Agent", "splitrail")
        .header("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(5))
        .send()
        .await?;

    if !response.status().is_success() {
        anyhow::bail!("GitHub API returned {}", response.status());
    }

    let release: GitHubRelease = response.simd_json().await?;
    Ok(release.tag_name)
}

/// Spawn background version check, returns status handle
pub fn spawn_version_check() -> Arc<Mutex<UpdateStatus>> {
    let status = Arc::new(Mutex::new(UpdateStatus::Checking));
    let status_clone = status.clone();

    tokio::spawn(async move {
        let result = check_for_updates().await;
        if let Ok(mut s) = status_clone.lock() {
            *s = result;
        }
    });

    status
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_version() {
        assert_eq!(parse_version("3.2.0"), Some((3, 2, 0)));
        assert_eq!(parse_version("v3.2.0"), Some((3, 2, 0)));
        assert_eq!(parse_version("v10.20.30"), Some((10, 20, 30)));
        assert_eq!(parse_version("0.0.1"), Some((0, 0, 1)));
        assert_eq!(parse_version("invalid"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn test_is_newer() {
        // Newer versions
        assert!(is_newer("v3.3.0", "3.2.0"));
        assert!(is_newer("3.2.1", "3.2.0"));
        assert!(is_newer("4.0.0", "3.9.9"));
        assert!(is_newer("v4.0.0", "v3.9.9"));

        // Same version
        assert!(!is_newer("3.2.0", "3.2.0"));
        assert!(!is_newer("v3.2.0", "v3.2.0"));
        assert!(!is_newer("v3.2.0", "3.2.0"));

        // Older versions
        assert!(!is_newer("3.1.0", "3.2.0"));
        assert!(!is_newer("2.9.9", "3.0.0"));
        assert!(!is_newer("v3.1.9", "v3.2.0"));

        // Invalid versions
        assert!(!is_newer("invalid", "3.2.0"));
        assert!(!is_newer("3.2.0", "invalid"));
    }
}
