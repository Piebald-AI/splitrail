//! Persistent record of what has already been exported.
//!
//! Timestamps are not enough to make an export idempotent: agent transcripts
//! get rewritten on resume and compaction, so usage can appear, move, or vanish
//! between two runs (see the fixes around #200/#204). The state file therefore
//! remembers the `global_hash` of every message that has been sent, which is the
//! same identity the upload path deduplicates on.
//!
//! The file lives in the platform state directory next to `state.toml`, so it is
//! runtime data rather than something a user edits.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const STATE_VERSION: u32 = 1;

/// Keep the state bounded. At ~40 bytes per entry this is a few MB, which is
/// worth it: losing entries means re-exporting spans, and re-exporting is only
/// safe because ids are deterministic — but duplicate cost counters are not.
const MAX_ENTRIES: usize = 500_000;

#[derive(Debug, Serialize, Deserialize)]
struct ExportState {
    version: u32,
    /// Traces endpoint these hashes were sent to.
    #[serde(default)]
    endpoint: String,
    /// Message and namespaced output hashes -> message timestamp in milliseconds.
    #[serde(default)]
    exported: BTreeMap<String, i64>,
    /// End of the last accepted metrics interval (Unix nanoseconds).
    #[serde(default)]
    metrics_endpoint_ns: Option<i64>,
}

impl Default for ExportState {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            endpoint: String::new(),
            exported: BTreeMap::new(),
            metrics_endpoint_ns: None,
        }
    }
}

impl ExportState {
    fn is_exported(&self, global_hash: &str) -> bool {
        self.exported.contains_key(global_hash)
    }

    fn record(&mut self, global_hash: &str, timestamp: DateTime<Utc>) {
        self.exported
            .insert(global_hash.to_string(), timestamp.timestamp_millis());
    }

    /// Drop the oldest entries once the state grows past [`MAX_ENTRIES`].
    fn prune(&mut self) {
        if self.exported.len() <= MAX_ENTRIES {
            return;
        }
        let mut entries: Vec<(String, i64)> = self
            .exported
            .iter()
            .map(|(hash, ts)| (hash.clone(), *ts))
            .collect();
        entries.sort_by_key(|(_, ts)| *ts);
        let drop_count = self.exported.len().saturating_sub(MAX_ENTRIES);
        let doomed: Vec<String> = entries
            .into_iter()
            .take(drop_count)
            .map(|(hash, _)| hash)
            .collect();
        for hash in doomed {
            self.exported.remove(&hash);
        }
    }
}

thread_local! {
    static TEST_STATE_PATH: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Test hook: point the export state at a temporary file.
#[cfg(test)]
pub fn set_test_state_path(path: PathBuf) {
    TEST_STATE_PATH.with(|slot| *slot.borrow_mut() = Some(path));
}

/// Location of the export state file.
pub fn state_path() -> Result<PathBuf> {
    #[cfg(test)]
    {
        if let Some(path) = TEST_STATE_PATH.with(|slot| slot.borrow().clone()) {
            return Ok(path);
        }
    }

    let root = dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .context("Could not find platform state directory")?;
    Ok(root.join("splitrail").join("otel-state.json"))
}

/// Loaded state plus the path it came from, saved only when a run succeeds.
pub struct ExportStateHandle {
    state: ExportState,
    path: PathBuf,
    endpoint: String,
    dirty: bool,
}

impl ExportStateHandle {
    /// Load the state file, starting fresh when it is missing or unreadable.
    ///
    /// A corrupt file is moved aside rather than fatal: refusing to export would
    /// be a worse failure than re-exporting with stable span ids.
    pub fn open(endpoint: &str) -> Result<Self> {
        let path = state_path()?;
        let state = match load_state(&path) {
            Ok(Some(state)) => state,
            Ok(None) => ExportState::default(),
            Err(error) => {
                let backup = path.with_extension("json.corrupt");
                let _ = fs::rename(&path, &backup);
                eprintln!(
                    "⚠️  Could not read {}: {error:#}. Starting with empty export state (moved to {}).",
                    path.display(),
                    backup.display()
                );
                ExportState::default()
            }
        };
        if !state.endpoint.is_empty() && state.endpoint != endpoint {
            eprintln!(
                "⚠️  Export state belongs to {}, not {endpoint}. Re-exporting nothing for messages it \
                 already recorded; delete {} if this collector should receive the full history.",
                state.endpoint,
                path.display()
            );
        }
        Ok(Self {
            state,
            path,
            endpoint: endpoint.to_string(),
            dirty: false,
        })
    }

    pub fn is_exported(&self, global_hash: &str) -> bool {
        self.state.is_exported(global_hash)
    }
    pub fn metrics_interval(&self, now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
        let previous = self.state.metrics_endpoint_ns.and_then(|ns| {
            DateTime::from_timestamp(
                ns.div_euclid(1_000_000_000),
                ns.rem_euclid(1_000_000_000) as u32,
            )
        });
        let end = previous.map_or(now, |previous| {
            now.max(previous + chrono::Duration::nanoseconds(1))
        });
        let start = previous.unwrap_or_else(|| end - chrono::Duration::nanoseconds(1));
        (start, end)
    }

    pub fn record_metrics_endpoint(&mut self, end: DateTime<Utc>) {
        self.state.metrics_endpoint_ns = end.timestamp_nanos_opt();
        self.dirty = true;
    }

    pub fn record(&mut self, global_hash: &str, timestamp: DateTime<Utc>) {
        self.state.record(global_hash, timestamp);
        self.dirty = true;
    }

    /// Persist the state. Only called after every batch of a run was accepted.
    pub fn commit(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.state.prune();
        self.state.endpoint = self.endpoint.clone();
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create state directory {}", parent.display())
            })?;
        }
        let bytes = simd_json::to_vec(&self.state).context("Failed to serialize export state")?;
        fs::write(&self.path, bytes)
            .with_context(|| format!("Failed to write {}", self.path.display()))?;
        self.dirty = false;
        Ok(())
    }
}

fn load_state(path: &std::path::Path) -> Result<Option<ExportState>> {
    if !path.exists() {
        return Ok(None);
    }
    let mut bytes = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let state: ExportState = simd_json::from_slice(&mut bytes)
        .with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn temp_state_path() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("splitrail-otel-state-test-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir.join(format!("state-{}.json", uuid_like()))
    }

    fn uuid_like() -> u128 {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    }

    fn timestamp() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap()
    }

    #[test]
    fn round_trips_exported_hashes() {
        let path = temp_state_path();
        set_test_state_path(path.clone());

        let mut handle = ExportStateHandle::open("http://collector:4318").unwrap();
        assert!(!handle.is_exported("hash-a"));
        handle.record("hash-a", timestamp());
        handle.commit().unwrap();

        let handle = ExportStateHandle::open("http://collector:4318").unwrap();
        assert!(handle.is_exported("hash-a"));
        assert!(!handle.is_exported("hash-b"));

        let _ = fs::remove_file(&path);
    }

    #[test]
    fn corrupt_state_is_replaced_not_fatal() {
        let path = temp_state_path();
        fs::write(&path, b"{ this is not json").unwrap();
        set_test_state_path(path.clone());

        let handle = ExportStateHandle::open("http://collector:4318").unwrap();
        assert!(!handle.is_exported("anything"));
        assert!(!path.exists());
        assert!(path.with_extension("json.corrupt").exists());

        let _ = fs::rename(path.with_extension("json.corrupt"), &path);
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn commit_is_a_noop_without_changes() {
        let path = temp_state_path();
        set_test_state_path(path.clone());
        let mut handle = ExportStateHandle::open("http://collector:4318").unwrap();
        handle.commit().unwrap();
        assert!(!path.exists(), "state file should not be created when idle");
    }

    #[test]
    fn prune_keeps_the_newest_entries() {
        let mut state = ExportState::default();
        for index in 0..(MAX_ENTRIES + 10) {
            state.exported.insert(format!("hash-{index}"), index as i64);
        }
        state.prune();
        assert_eq!(state.exported.len(), MAX_ENTRIES);
        assert!(!state.exported.contains_key("hash-0"));
        assert!(
            state
                .exported
                .contains_key(&format!("hash-{}", MAX_ENTRIES + 9))
        );
    }
}
