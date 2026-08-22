//! Settings loader — reads settings.json directly from the app data directory.
//!
//! Path: `%APPDATA%/gg.clipsta.desktop/settings.json`
//!
//! This allows clipsta-capture.exe to start independently without the Tauri process.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Full settings schema matching the Tauri/Electron version exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppSettings {
    pub output_folder: String,
    pub hotkey_clip30_sec: String,
    pub hotkey_clip1_min: String,
    pub hotkey_clip5_min: String,
    pub hotkey_record: String,
    pub buffer_duration: u32,
    pub resolution: String,
    pub fps: u32,
    pub aspect_ratio: String,
    pub encoder: String,
    pub quality: String,
    pub bitrate: u32,
    pub audio_bitrate: u32,
    pub capture_audio: bool,
    pub capture_mic: bool,
    pub audio_source: String,
    pub audio_input_device_id: String,
    pub game_detect: bool,
    pub auto_upload: bool,
    pub minimize_to_tray: bool,
    pub overlay_enabled: bool,
    pub clip_sound_enabled: bool,
    pub cloud_enabled: bool,
    pub cloud_pair_code: String,
    pub upload_bandwidth: u32,
    pub delete_after_upload: bool,
    pub desktop_device_id: String,
    pub desktop_audio_device_id: String,
    pub watch_folder_path: String,
    pub watch_folder_enabled: bool,
    pub theme: String,
    pub multi_track_audio: bool,
    pub start_at_login: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        let output = dirs::video_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("Clipsta");
        Self {
            output_folder: output.to_string_lossy().to_string(),
            hotkey_clip30_sec: "Ctrl+Shift+G".to_string(),
            hotkey_clip1_min: "Alt+F9".to_string(),
            hotkey_clip5_min: "Alt+F10".to_string(),
            hotkey_record: "F9".to_string(),
            buffer_duration: 60,
            resolution: "1080p".to_string(),
            fps: 60,
            aspect_ratio: "16:9".to_string(),
            encoder: "auto".to_string(),
            quality: "high".to_string(),
            bitrate: 8000,
            audio_bitrate: 192,
            capture_audio: true,
            capture_mic: false,
            audio_source: "both".to_string(),
            audio_input_device_id: String::new(),
            game_detect: true,
            auto_upload: false,
            minimize_to_tray: true,
            overlay_enabled: true,
            clip_sound_enabled: true,
            cloud_enabled: false,
            cloud_pair_code: String::new(),
            upload_bandwidth: 0,
            delete_after_upload: false,
            desktop_device_id: String::new(),
            desktop_audio_device_id: String::new(),
            watch_folder_path: String::new(),
            watch_folder_enabled: false,
            theme: "dark".to_string(),
            multi_track_audio: false,
            start_at_login: false,
        }
    }
}

/// Return the app data directory: `%APPDATA%/gg.clipsta.desktop`
pub fn settings_dir() -> PathBuf {
    let roaming = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::config_dir().unwrap_or_else(|| PathBuf::from("."))
        });
    roaming.join("gg.clipsta.desktop")
}

/// Load settings from `%APPDATA%/gg.clipsta.desktop/settings.json`.
/// Returns defaults if the file doesn't exist or can't be parsed.
pub fn load() -> AppSettings {
    let path = settings_dir().join("settings.json");
    if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(json) => serde_json::from_str::<AppSettings>(&json).unwrap_or_default(),
            Err(_) => AppSettings::default(),
        }
    } else {
        AppSettings::default()
    }
}

/// Convert a resolution string to (width, height) for the encoder.
/// Returns `None` for "native" — capture uses the source's native dimensions.
/// Heights are 16-pixel aligned where needed for hardware encoder compatibility.
pub fn resolution_to_dimensions(resolution: &str) -> (Option<u32>, Option<u32>) {
    match resolution {
        "480p" => (Some(854), Some(480)),
        "720p" => (Some(1280), Some(720)),
        "1080p" => (Some(1920), Some(1088)), // 1088 = 16-aligned
        "1440p" => (Some(2560), Some(1440)),
        "4k" | "2160p" => (Some(3840), Some(2160)),
        _ => (None, None), // "native" or unknown
    }
}

/// Resolve bitrate (kbps) based on resolution, fps, and quality preset.
///
/// Bitrates are tuned to industry standards:
/// - standard: Efficient, smaller files. Good for sharing/upload.
/// - high: Matches ShadowPlay/ReLive defaults. Best balance of quality and size.
/// - ultra: Maximum clarity. Matches OBS "Indistinguishable" quality. Large files.
pub fn resolve_bitrate_kbps(resolution: &str, fps: u32, quality: &str) -> u32 {
    let is60 = fps >= 50;
    match quality {
        "standard" | "low" => match resolution {
            "480p" => if is60 { 2500 } else { 1500 },
            "720p" => if is60 { 5000 } else { 3000 },
            "1080p" => if is60 { 12000 } else { 8000 },
            "1440p" => if is60 { 30000 } else { 20000 },
            "4k" | "2160p" => if is60 { 50000 } else { 35000 },
            _ => if is60 { 12000 } else { 8000 },
        },
        "high" | "medium" => match resolution {
            "480p" => if is60 { 4000 } else { 2500 },
            "720p" => if is60 { 8000 } else { 5000 },
            "1080p" => if is60 { 20000 } else { 12000 },
            "1440p" => if is60 { 50000 } else { 30000 },
            "4k" | "2160p" => if is60 { 80000 } else { 50000 },
            _ => if is60 { 20000 } else { 12000 },
        },
        "ultra" => match resolution {
            "480p" => if is60 { 8000 } else { 5000 },
            "720p" => if is60 { 15000 } else { 10000 },
            "1080p" => if is60 { 35000 } else { 25000 },
            "1440p" => if is60 { 80000 } else { 55000 },
            "4k" | "2160p" => if is60 { 130000 } else { 90000 },
            _ => if is60 { 35000 } else { 25000 },
        },
        _ => resolve_bitrate_kbps(resolution, fps, "high"),
    }
}
