//! Lightweight file logging for clipsta-capture.exe
//!
//! Design goals:
//! - Zero performance impact on the capture hot path (async background writer)
//! - Rotating log files (max 5 MB each, keep last 3)
//! - Logs to %APPDATA%/Clipsta/logs/capture.log
//! - Also prints to stderr for debugging
//! - No external crate dependencies

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{self, SyncSender};
use std::sync::OnceLock;

/// Maximum log file size before rotation (5 MB)
const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;
/// Number of rotated log files to keep
const MAX_LOG_FILES: usize = 3;

static LOG_TX: OnceLock<SyncSender<String>> = OnceLock::new();

/// Initialize the logging system. Call once at startup.
pub fn init() {
    let (tx, rx) = mpsc::sync_channel::<String>(256);
    LOG_TX.set(tx).ok();

    // Background writer thread — never blocks the capture pipeline
    std::thread::Builder::new()
        .name("log-writer".to_string())
        .spawn(move || {
            let log_dir = log_dir();
            let _ = fs::create_dir_all(&log_dir);
            let log_path = log_dir.join("capture.log");

            let mut file = open_log_file(&log_path);

            for msg in rx {
                if let Some(ref mut f) = file {
                    let _ = writeln!(f, "{}", msg);
                    // Check size and rotate if needed
                    if let Ok(meta) = f.metadata() {
                        if meta.len() > MAX_LOG_SIZE {
                            drop(file.take());
                            rotate_logs(&log_path);
                            file = open_log_file(&log_path);
                        }
                    }
                }
            }
        })
        .ok();
}

/// Log a formatted message (non-blocking). Also prints to stderr.
#[allow(dead_code)]
pub fn log(fmt: &str, args: impl std::fmt::Display) {
    let timestamp = chrono_timestamp();
    let msg = fmt.replace("{}", &args.to_string());
    let line = format!("{} {}", timestamp, msg);
    eprintln!("{}", msg);
    if let Some(tx) = LOG_TX.get() {
        let _ = tx.try_send(line);
    }
}

/// Macro-free multi-arg logging helper.
/// Usage: logging::log_args(format_args!("[tag] {} at {}", val1, val2));
#[allow(dead_code)]
pub fn log_args(args: std::fmt::Arguments<'_>) {
    let timestamp = chrono_timestamp();
    let msg = format!("{}", args);
    let line = format!("{} {}", timestamp, msg);
    eprintln!("{}", msg);
    if let Some(tx) = LOG_TX.get() {
        let _ = tx.try_send(line);
    }
}

/// Get the log directory path.
fn log_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Clipsta")
        .join("logs")
}

/// Open the log file for appending.
fn open_log_file(path: &PathBuf) -> Option<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// Rotate log files: capture.log → capture.1.log → capture.2.log → delete oldest
fn rotate_logs(path: &PathBuf) {
    let dir = path.parent().unwrap_or(path);
    // Delete the oldest
    let oldest = dir.join(format!("capture.{}.log", MAX_LOG_FILES));
    let _ = fs::remove_file(&oldest);
    // Shift existing files
    for i in (1..MAX_LOG_FILES).rev() {
        let from = dir.join(format!("capture.{}.log", i));
        let to = dir.join(format!("capture.{}.log", i + 1));
        let _ = fs::rename(&from, &to);
    }
    // Move current to .1
    let first = dir.join("capture.1.log");
    let _ = fs::rename(path, &first);
}

/// Simple timestamp without pulling in chrono (uses system time)
fn chrono_timestamp() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    // Simple ISO-ish timestamp: seconds since epoch (compact, sortable)
    // For human-readable, compute H:M:S from remainder of day
    let day_secs = (secs % 86400) as u32;
    let h = day_secs / 3600;
    let m = (day_secs % 3600) / 60;
    let s = day_secs % 60;
    let ms = now.subsec_millis();
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}
