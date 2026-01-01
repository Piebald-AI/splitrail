//! Debug logging for diagnosing lock contention issues.
//!
//! Enable by setting environment variable: SPLITRAIL_DEBUG_LOG=1
//! Logs are written to /tmp/splitrail-debug.log

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

static ENABLED: AtomicBool = AtomicBool::new(false);
static START_TIME: OnceLock<Instant> = OnceLock::new();
static LOG_FILE: OnceLock<std::sync::Mutex<std::fs::File>> = OnceLock::new();

/// Initialize debug logging. Call once at startup.
pub fn init() {
    if std::env::var("SPLITRAIL_DEBUG_LOG").is_ok() {
        ENABLED.store(true, Ordering::SeqCst);
        START_TIME.get_or_init(Instant::now);
        LOG_FILE.get_or_init(|| {
            let file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open("/tmp/splitrail-debug.log")
                .expect("Failed to open debug log file");
            std::sync::Mutex::new(file)
        });
        log("DEBUG", "init", "Debug logging initialized");
    }
}

/// Check if debug logging is enabled.
#[inline]
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Log a debug message with timestamp and thread ID.
pub fn log(category: &str, action: &str, detail: &str) {
    if !is_enabled() {
        return;
    }

    let elapsed = START_TIME
        .get()
        .map(|s| s.elapsed().as_millis())
        .unwrap_or(0);
    let thread_id = std::thread::current().id();

    let msg = format!(
        "[{:>8}ms] [{:?}] [{}] {} - {}\n",
        elapsed, thread_id, category, action, detail
    );

    if let Some(file_mutex) = LOG_FILE.get()
        && let Ok(mut file) = file_mutex.lock()
    {
        let _ = file.write_all(msg.as_bytes());
        let _ = file.flush();
    }
}

/// Log a lock acquisition attempt.
#[inline]
pub fn lock_acquiring(lock_type: &str, view_name: &str) {
    if is_enabled() {
        log(lock_type, "ACQUIRING", view_name);
    }
}

/// Log a successful lock acquisition.
#[inline]
pub fn lock_acquired(lock_type: &str, view_name: &str) {
    if is_enabled() {
        log(lock_type, "ACQUIRED", view_name);
    }
}

/// Log a lock release.
#[inline]
pub fn lock_released(lock_type: &str, view_name: &str) {
    if is_enabled() {
        log(lock_type, "RELEASED", view_name);
    }
}

/// RAII guard that logs when dropped.
pub struct LogOnDrop {
    lock_type: &'static str,
    view_name: String,
}

impl LogOnDrop {
    pub fn new(lock_type: &'static str, view_name: String) -> Self {
        Self {
            lock_type,
            view_name,
        }
    }
}

impl Drop for LogOnDrop {
    fn drop(&mut self) {
        lock_released(self.lock_type, &self.view_name);
    }
}
