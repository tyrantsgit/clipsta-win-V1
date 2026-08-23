//! Tauri commands — all IPC handlers for the frontend.

use std::path::PathBuf;
use std::process::Command;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_global_shortcut::GlobalShortcutExt;

use crate::capture_proxy::CaptureProxy;
use clipsta_capture::gpu_capture::SourceInfo;
use clipsta_capture::ipc::StartPayload;
use crate::settings::{AppSettings, SettingsStore};
use std::sync::Arc;

/// Initialize COM MTA on the current thread (for async command threads).
/// Safe to call multiple times — returns S_FALSE if already initialized.
fn ensure_com() {
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

// ── Settings commands ─────────────────────────────────────────────────────────

#[tauri::command]
pub async fn settings_get_all(store: State<'_, SettingsStore>) -> Result<AppSettings, String> {
    Ok(store.get())
}

#[tauri::command]
pub async fn settings_set(
    app: AppHandle,
    store: State<'_, SettingsStore>,
    key: String,
    value: serde_json::Value,
) -> Result<bool, String> {
    store.set_field(&key, value);
    // Re-register hotkeys if a hotkey field was changed
    if key.starts_with("hotkey") {
        crate::register_hotkeys(&app, &store.get());
    }
    Ok(true)
}

#[tauri::command]
pub async fn settings_set_all(
    app: AppHandle,
    store: State<'_, SettingsStore>,
    settings: serde_json::Value,
) -> Result<bool, String> {
    store.set_all(settings);
    // Re-register global hotkeys with the updated settings
    crate::register_hotkeys(&app, &store.get());
    Ok(true)
}

// ── Clip management commands ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipFile {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub created_at: String,
}

#[tauri::command]
pub async fn clips_list(store: State<'_, SettingsStore>) -> Result<Vec<ClipFile>, String> {
    let settings = store.get();
    let folder = PathBuf::from(&settings.output_folder);
    if !folder.exists() {
        return Ok(Vec::new());
    }
    // Run the recursive filesystem scan off the async executor thread
    tokio::task::spawn_blocking(move || {
        let mut clips = Vec::new();
        fn scan_dir(dir: &std::path::Path, clips: &mut Vec<ClipFile>, depth: u32) {
            // Max depth of 3 prevents runaway scanning if output folder is misconfigured
            if depth > 3 { return; }
            let Ok(entries) = std::fs::read_dir(dir) else { return };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    scan_dir(&path, clips, depth + 1);
                    continue;
                }
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !matches!(ext.as_str(), "mp4" | "webm" | "mkv" | "mov") {
                    continue;
                }
                if let Ok(meta) = entry.metadata() {
                    if meta.len() == 0 { continue; }
                    let created = meta
                        .created()
                        .map(|t| {
                            let dt: chrono::DateTime<chrono::Local> = t.into();
                            dt.to_rfc3339()
                        })
                        .unwrap_or_default();
                    clips.push(ClipFile {
                        name: path.file_name().unwrap_or_default().to_string_lossy().to_string(),
                        path: path.to_string_lossy().to_string(),
                        size: meta.len(),
                        created_at: created,
                    });
                }
            }
        }
        scan_dir(&folder, &mut clips, 0);
        clips.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        clips
    })
    .await
    .map_err(|e| format!("clips_list task failed: {}", e))
}

#[tauri::command]
pub async fn clips_delete(path: String, store: State<'_, SettingsStore>) -> Result<bool, String> {
    validate_clip_path(&path, &store)?;
    if std::path::Path::new(&path).exists() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(true)
}

#[tauri::command]
pub async fn clips_rename(old_path: String, new_name: String, store: State<'_, SettingsStore>) -> Result<String, String> {
    validate_clip_path(&old_path, &store)?;
    // Sanitize new_name: no path separators allowed
    if new_name.contains('/') || new_name.contains('\\') || new_name.contains("..") {
        return Err("Invalid file name".to_string());
    }
    let dir = std::path::Path::new(&old_path)
        .parent()
        .ok_or("No parent dir")?;
    let new_path = dir.join(&new_name);
    std::fs::rename(&old_path, &new_path).map_err(|e| e.to_string())?;
    Ok(new_path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn clips_import(
    source_path: String,
    store: State<'_, SettingsStore>,
) -> Result<String, String> {
    // Validate source is a video file
    let ext = std::path::Path::new(&source_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !matches!(ext.as_str(), "mp4" | "webm" | "mkv" | "mov") {
        return Err("Only video files (mp4, webm, mkv, mov) can be imported".to_string());
    }
    if source_path.starts_with("\\\\") || source_path.contains("..") {
        return Err("Invalid source path".to_string());
    }
    let folder = ensure_output_folder(&store);
    let name = std::path::Path::new(&source_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let dest = unique_path(&folder, &name);
    let dest_clone = dest.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::copy(&source_path, &dest_clone).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("import task failed: {}", e))??;
    Ok(dest.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn clips_import_folder(
    source_folder: String,
    store: State<'_, SettingsStore>,
) -> Result<Vec<String>, String> {
    let folder = ensure_output_folder(&store);
    tokio::task::spawn_blocking(move || {
        let entries = std::fs::read_dir(&source_folder).map_err(|e| e.to_string())?;
        let mut imported = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            if !matches!(ext.as_str(), "mp4" | "webm" | "mkv" | "mov") {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            let dest = unique_path(&folder, &name);
            if std::fs::copy(&path, &dest).is_ok() {
                imported.push(dest.to_string_lossy().to_string());
            }
        }
        Ok(imported)
    })
    .await
    .map_err(|e| format!("import folder task failed: {}", e))?
}


// ── Recording control commands ────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartRecordingOpts {
    pub source_id: Option<String>,
    pub fps: Option<u32>,
    pub no_audio: Option<bool>,
    pub mic_device: Option<String>,
    pub loopback_device: Option<String>,
}

#[tauri::command]
pub async fn wgc_sources() -> Result<Vec<SourceInfo>, String> {
    ensure_com();
    Ok(clipsta_capture::gpu_capture::list_sources())
}

#[tauri::command]
pub async fn wgc_capture_diagnostics() -> Result<clipsta_capture::gpu_capture::CaptureDiagnostics, String> {
    ensure_com();
    Ok(clipsta_capture::gpu_capture::capture_diagnostics())
}

#[tauri::command]
pub async fn wgc_start_recording(
    app: AppHandle,
    proxy: State<'_, Arc<CaptureProxy>>,
    store: State<'_, SettingsStore>,
    opts: StartRecordingOpts,
) -> Result<serde_json::Value, String> {
    let settings = store.get();
    let fps = opts.fps.unwrap_or(settings.fps);
    let no_audio = opts.no_audio.unwrap_or(!settings.capture_audio);

    // Resolve output resolution and bitrate from user settings + quality preset
    let dims = resolution_to_dimensions(&settings.resolution);
    let (target_w, target_h) = match dims {
        Some((w, h)) => (Some(w), Some(h)),
        None => (None, None), // "native" — use source dimensions
    };
    let bitrate = resolve_quality_bitrate(&settings.resolution, fps, &settings.quality);

    let payload = StartPayload {
        source_id: opts.source_id,
        fps,
        no_audio,
        mic_device: opts.mic_device.or_else(|| {
            let wants_mic = settings.audio_source == "mic" || settings.audio_source == "both" || settings.capture_mic;
            if wants_mic {
                if settings.audio_input_device_id.is_empty() { Some("default".to_string()) }
                else { Some(settings.audio_input_device_id.clone()) }
            } else {
                None
            }
        }),
        loopback_device: opts.loopback_device.or_else(|| {
            if settings.desktop_audio_device_id.is_empty() { None }
            else { Some(settings.desktop_audio_device_id.clone()) }
        }),
        target_width: target_w,
        target_height: target_h,
        bitrate_kbps: bitrate,
        buffer_duration: settings.buffer_duration,
        multi_track_audio: settings.multi_track_audio,
    };

    let info = proxy.start(payload).map_err(|e| {
        let msg = format!("Capture start failed: {}", e);
        let _ = app.emit("wgc:error", &msg);
        msg
    })?;

    Ok(serde_json::json!({
        "width": info.width,
        "height": info.height,
        "fps": info.fps,
        "segmentDir": "",
    }))
}

#[tauri::command]
pub async fn wgc_stop_recording(proxy: State<'_, Arc<CaptureProxy>>) -> Result<(), String> {
    proxy.stop().map_err(|e| e)
}

#[tauri::command]
pub async fn wgc_save_clip(
    app: AppHandle,
    proxy: State<'_, Arc<CaptureProxy>>,
    store: State<'_, SettingsStore>,
    seconds: u32,
    file_name: String,
) -> Result<Option<String>, String> {
    if proxy.is_saving.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("Another save is in progress".to_string());
    }

    if !proxy.is_recording.load(std::sync::atomic::Ordering::Relaxed) {
        return Ok(None);
    }

    let output_folder = ensure_output_folder(&store);
    // Extract game name from the ShadowPlay-style filename.
    // Filename format: "GameName YYYY.MM.DD - HH.MM.SS.ff.DVR.mp4"
    // Split on the date pattern (4 digits = year) to isolate the game name prefix.
    let game_name = {
        // Find the first occurrence of a 4-digit year pattern (e.g., "2026.")
        let raw = file_name
            .find(|c: char| c.is_ascii_digit())
            .and_then(|idx| {
                // Check if this looks like a date (digit followed by more digits and dots)
                let rest = &file_name[idx..];
                if rest.len() >= 4 && rest[..4].chars().all(|c| c.is_ascii_digit()) {
                    Some(file_name[..idx].trim().to_string())
                } else {
                    // Single digit in game name (e.g., "Battlefield 6") — skip to next digit group
                    None
                }
            })
            .unwrap_or_else(|| {
                // Fallback: split on first digit
                file_name
                    .split(|c: char| c.is_ascii_digit())
                    .next()
                    .unwrap_or("Desktop")
                    .trim()
                    .to_string()
            });
        raw
    };
    // Only create a game subfolder if the name looks like an actual game/app
    // (not a browser page title, not "Desktop", not empty).
    // Browser titles are stripped by get_active_window_title but may still
    // contain page titles. Skip folder creation for common non-game patterns.
    let is_desktop = game_name.is_empty()
        || game_name == "Desktop"
        || game_name == "Clipsta"
        || game_name.contains(" - ");  // Browser page titles typically have " - "
    let game_folder = if is_desktop {
        output_folder.clone()
    } else {
        let gf = output_folder.join(&game_name);
        let _ = std::fs::create_dir_all(&gf);
        gf
    };
    let output_path = game_folder.join(&file_name);
    let output_str = output_path.to_string_lossy().to_string();

    // Send save command to capture process via IPC
    match proxy.save_clip(seconds, &output_str) {
        Ok(path) => {
            eprintln!("[clipsta] Clip saved: {}", path);
            // Generate thumbnail in background (non-blocking — clip is already saved)
            let thumb_path = path.clone();
            std::thread::spawn(move || {
                generate_thumbnail(&thumb_path);
            });
            let _ = app.emit("wgc:clipSaved", &path);
            let settings = store.get();
            if settings.clip_sound_enabled {
                let _ = app.emit("play-clip-sound", ());
            }
            Ok(Some(path))
        }
        Err(e) => {
            if e.contains("Not enough") || e.contains("No keyframe") {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

#[tauri::command]
pub async fn wgc_save_full_recording(
    app: AppHandle,
    proxy: State<'_, Arc<CaptureProxy>>,
    store: State<'_, SettingsStore>,
) -> Result<Option<String>, String> {
    // Save the entire buffer content (up to buffer_duration seconds)
    let settings = store.get();
    let output_folder = ensure_output_folder(&store);
    let stamp = chrono::Local::now().format("%Y.%m.%d - %H.%M.%S.00");
    let output_path = output_folder.join(format!("Desktop {}.DVR.mp4", stamp));
    let output_str = output_path.to_string_lossy().to_string();

    // Save the maximum buffer duration
    match proxy.save_clip(settings.buffer_duration, &output_str) {
        Ok(path) => {
            let _ = app.emit("wgc:clipSaved", &path);
            Ok(Some(path))
        }
        Err(e) => {
            if e.contains("Not enough") || e.contains("No keyframe") {
                Ok(None)
            } else {
                Err(e)
            }
        }
    }
}

// ── File operation commands ───────────────────────────────────────────────────

#[tauri::command]
pub async fn shell_open_folder(path: String) -> Result<(), String> {
    // Reject UNC paths and special devices
    if path.starts_with("\\\\") || path.contains("..") {
        return Err("Invalid path".to_string());
    }
    Command::new("explorer")
        .arg(&path)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn shell_open_file(path: String) -> Result<(), String> {
    if path.starts_with("\\\\") || path.contains("..") {
        return Err("Invalid path".to_string());
    }
    Command::new("explorer")
        .arg(&path)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn shell_show_item(path: String) -> Result<(), String> {
    if path.starts_with("\\\\") || path.contains("..") {
        return Err("Invalid path".to_string());
    }
    Command::new("explorer")
        .args(["/select,", &path])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn file_stat(file_path: String, store: State<'_, SettingsStore>) -> Result<serde_json::Value, String> {
    validate_accessible_path(&file_path, &store)?;
    let meta = std::fs::metadata(&file_path).map_err(|e| e.to_string())?;
    let modified = meta
        .modified()
        .map(|t| {
            let dt: chrono::DateTime<chrono::Local> = t.into();
            dt.to_rfc3339()
        })
        .unwrap_or_default();
    Ok(serde_json::json!({
        "size": meta.len(),
        "modifiedAt": modified,
    }))
}

#[tauri::command]
pub async fn file_ensure_dir(dir_path: String) -> Result<bool, String> {
    // Only allow creating dirs under known safe locations
    if dir_path.starts_with("\\\\") || dir_path.contains("..") {
        return Err("Invalid directory path".to_string());
    }
    let path = std::path::Path::new(&dir_path);
    let allowed = [
        dirs::video_dir(),
        dirs::download_dir(),
        dirs::home_dir().map(|h| h.join("Videos")),
        Some(std::env::temp_dir()),
    ];
    let is_allowed = allowed.iter().flatten().any(|d| path.starts_with(d));
    if !is_allowed {
        // Also allow if it's under AppData
        let appdata_ok = dirs::data_dir().map(|d| path.starts_with(d)).unwrap_or(false);
        if !appdata_ok {
            return Err("Directory path not in allowed location".to_string());
        }
    }
    std::fs::create_dir_all(&dir_path).map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub async fn file_copy_to_downloads(file_path: String, store: State<'_, SettingsStore>) -> Result<String, String> {
    validate_accessible_path(&file_path, &store)?;
    let downloads = dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")));
    let name = std::path::Path::new(&file_path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let dest = unique_path(&downloads, &name);
    std::fs::copy(&file_path, &dest).map_err(|e| e.to_string())?;
    Ok(dest.to_string_lossy().to_string())
}


// ── Export command ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOpts {
    pub format: Option<String>,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
    pub encoder: Option<String>,
    pub fps: Option<u32>,
    pub trim_start: Option<f64>,
    pub trim_end: Option<f64>,
    pub cuts: Option<Vec<CutRange>>,
    pub brightness: Option<u32>,
    pub contrast: Option<u32>,
    pub saturation: Option<u32>,
    pub speed_segments: Option<Vec<SpeedSegmentOpts>>,
    pub transitions: Option<Vec<TransitionOpts>>,
    pub timeline: Option<Vec<TimelineClip>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineClip {
    pub path: String,
    pub trim_in: f64,
    pub trim_out: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeedSegmentOpts {
    pub start: f64,
    pub end: f64,
    pub speed: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransitionOpts {
    pub time: f64,
    #[serde(rename = "type")]
    pub transition_type: String,
    pub duration: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CutRange {
    pub start: f64,
    pub end: f64,
}

/// Compress a clip to 720p for faster upload.
/// Returns Ok(None) — direct upload of original quality is the preferred path
/// (constraint #11: clips are already encoded at target resolution by the
/// capture pipeline, so re-encoding for upload is unnecessary overhead).
/// If a smaller upload size is needed in the future, this can be implemented
/// with MF Sink Writer + hardware H.264 encoder at 720p.
#[tauri::command]
pub async fn compress_for_upload(file_path: String) -> Result<Option<String>, String> {
    let _ = file_path;
    Ok(None)
}

/// Upload a clip entirely in Rust (avoids WebView2 memory crash from 100MB+ fetch).
/// The frontend calls this instead of reading the file + doing fetch() in JavaScript.
#[tauri::command]
pub async fn native_upload_clip(
    store: State<'_, SettingsStore>,
    file_path: String,
) -> Result<String, String> {
    let settings = store.get();
    let device_id = settings.desktop_device_id.clone();

    tokio::task::spawn_blocking(move || {
        crate::do_rust_upload(&file_path, &device_id)
    })
    .await
    .map_err(|e| format!("Upload task failed: {}", e))?
    .map(|_| "Upload complete".to_string())
}

#[tauri::command]
pub async fn recording_export(
    app: AppHandle,
    input_path: String,
    output_path: String,
    opts: ExportOpts,
) -> Result<String, String> {
    let output_clone = output_path.clone();

    tokio::task::spawn_blocking(move || {
        ffmpeg_export_with_progress(&app, &input_path, &output_path, &opts)
    })
    .await
    .map_err(|e| format!("Export task failed: {}", e))?
    .map_err(|e| format!("Export failed: {}", e))?;

    Ok(output_clone)
}

/// Wrapper that runs FFmpeg with real-time progress parsing from stderr.
/// Replaces the old estimation-based approach with actual FFmpeg progress.
fn ffmpeg_export_with_progress(app: &AppHandle, input: &str, output: &str, opts: &ExportOpts) -> Result<(), String> {
    // Multi-clip timeline merge: use concat approach
    if let Some(ref timeline) = opts.timeline {
        if timeline.len() > 1 {
            let result = ffmpeg_concat_timeline(app, timeline, output, opts);
            let _ = app.emit("export:progress", 100u32);
            return result;
        }
    }

    // Single clip path
    let input_duration = get_video_duration(input).unwrap_or(30.0);
    let effective_duration = if let (Some(start), Some(end)) = (opts.trim_start, opts.trim_end) {
        end - start
    } else {
        input_duration
    };

    let result = ffmpeg_export_piped(app, input, output, opts, effective_duration);

    // Emit 100% on completion
    let _ = app.emit("export:progress", 100u32);

    result
}

/// Multi-clip timeline merge via FFmpeg filter_complex concat.
/// Each clip gets trimmed to its trimIn/trimOut, then all clips are concatenated.
fn ffmpeg_concat_timeline(app: &AppHandle, timeline: &[TimelineClip], output: &str, opts: &ExportOpts) -> Result<(), String> {
    use std::io::BufRead;
    use std::os::windows::process::CommandExt;

    let ffmpeg_path = find_ffmpeg().ok_or_else(|| "FFmpeg not found".to_string())?;

    // Calculate total duration for progress
    let total_duration: f64 = timeline.iter().map(|c| {
        let dur = c.trim_out - c.trim_in;
        if dur > 0.0 { dur } else { get_video_duration(&c.path).unwrap_or(30.0) }
    }).sum();

    let n = timeline.len();
    let mut args: Vec<String> = vec!["-y".to_string()];

    // Add all inputs WITHOUT -ss/-t (trimming handled in filter_complex)
    for clip in timeline {
        args.push("-i".to_string());
        args.push(clip.path.clone());
    }

    // Build filter_complex for concat with per-clip trimming
    let target_res = match opts.resolution.as_deref() {
        Some("480p") => "854:480",
        Some("720p") => "1280:720",
        Some("1440p") => "2560:1440",
        Some("4k") => "3840:2160",
        _ => "1920:1080",
    };

    let mut filter_parts: Vec<String> = Vec::new();
    let mut concat_inputs = String::new();

    for i in 0..n {
        let clip = &timeline[i];
        // Only apply trim filter if user actually set markers (trimIn > 0 or trimOut is less than a reasonable full-file assumption)
        // If trimOut >= 9000, it means "full file" — skip trim entirely
        // If trimIn == 0 and trimOut was set by auto-probe to full duration, we still trim to be precise
        let has_real_trim = clip.trim_in > 0.1 || (clip.trim_out > 0.0 && clip.trim_out < 9000.0);
        
        if has_real_trim && clip.trim_out < 9000.0 {
            let trim_start = clip.trim_in;
            let trim_end = clip.trim_out;
            // Video: trim with explicit start/end, then reset PTS, then scale
            filter_parts.push(format!(
                "[{i}:v]trim=start={:.3}:end={:.3},setpts=PTS-STARTPTS,scale={target_res}:force_original_aspect_ratio=decrease,pad={target_res}:(ow-iw)/2:(oh-ih)/2,setsar=1[v{i}]",
                trim_start, trim_end
            ));
            // Audio: atrim with explicit start/end
            filter_parts.push(format!(
                "[{i}:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo[a{i}]",
                trim_start, trim_end
            ));
        } else {
            // No trim — just scale
            filter_parts.push(format!(
                "[{i}:v]scale={target_res}:force_original_aspect_ratio=decrease,pad={target_res}:(ow-iw)/2:(oh-ih)/2,setsar=1[v{i}]"
            ));
            filter_parts.push(format!(
                "[{i}:a]aformat=sample_rates=48000:channel_layouts=stereo[a{i}]"
            ));
        }
        concat_inputs.push_str(&format!("[v{i}][a{i}]"));
    }

    let filter_complex = format!(
        "{};{}concat=n={}:v=1:a=1[vout][aout]",
        filter_parts.join(";"),
        concat_inputs,
        n
    );

    args.push("-filter_complex".to_string());
    args.push(filter_complex);
    args.push("-map".to_string());
    args.push("[vout]".to_string());
    args.push("-map".to_string());
    args.push("[aout]".to_string());

    // Framerate
    args.push("-r".to_string());
    args.push(format!("{}", opts.fps.unwrap_or(60)));

    // Video codec — try NVENC first
    args.push("-c:v".to_string());
    args.push("h264_nvenc".to_string());
    args.push("-preset".to_string());
    args.push("p7".to_string());
    args.push("-rc".to_string());
    args.push("vbr".to_string());
    args.push("-cq".to_string());
    args.push("18".to_string());
    args.push("-b:v".to_string());
    args.push("20M".to_string());

    // Audio codec
    args.push("-c:a".to_string());
    args.push("aac".to_string());
    args.push("-b:a".to_string());
    args.push("192k".to_string());

    args.push(output.to_string());

    // Run FFmpeg
    let mut child = std::process::Command::new(&ffmpeg_path)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| format!("Failed to run FFmpeg for merge: {}", e))?;

    // Parse progress
    let mut last_error_line = String::new();
    if let Some(stderr) = child.stderr.take() {
        let reader = std::io::BufReader::new(stderr);
        let mut last_pct: u32 = 0;
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if let Some(time_pos) = line.find("time=") {
                let time_str = &line[time_pos + 5..];
                if let Some(end) = time_str.find(|c: char| c == ' ' || c == '\r') {
                    let time_val = &time_str[..end];
                    if let Some(secs) = parse_ffmpeg_time(time_val) {
                        let pct = if total_duration > 0.0 {
                            ((secs / total_duration) * 100.0).min(99.0) as u32
                        } else { 0 };
                        if pct != last_pct {
                            last_pct = pct;
                            let _ = app.emit("export:progress", pct);
                        }
                    }
                }
            }
            if line.contains("Error") || line.contains("error") || line.contains("Cannot") || line.contains("Invalid") {
                last_error_line = line.clone();
            }
        }
    }

    let status = child.wait().map_err(|e| format!("FFmpeg merge error: {}", e))?;

    if !status.success() {
        // Fallback to software encoder if NVENC fails
        if last_error_line.contains("nvenc") || last_error_line.contains("Cannot load") || last_error_line.contains("No NVENC") {
            return ffmpeg_concat_timeline_software(timeline, output, opts, total_duration, app);
        }
        // Fallback: if audio stream missing, try without audio
        if last_error_line.contains("does not contain any stream") || last_error_line.contains("Stream map") {
            return ffmpeg_concat_timeline_video_only(timeline, output, opts, total_duration, app);
        }
        return Err(format!("Merge failed: {}", if last_error_line.is_empty() { "FFmpeg error".to_string() } else { last_error_line }));
    }

    Ok(())
}

/// Software fallback for multi-clip concat (no NVENC).
fn ffmpeg_concat_timeline_software(timeline: &[TimelineClip], output: &str, opts: &ExportOpts, total_duration: f64, app: &AppHandle) -> Result<(), String> {
    use std::io::BufRead;
    use std::os::windows::process::CommandExt;

    let ffmpeg_path = find_ffmpeg().ok_or_else(|| "FFmpeg not found".to_string())?;
    let n = timeline.len();
    let mut args: Vec<String> = vec!["-y".to_string()];

    for clip in timeline {
        args.push("-i".to_string());
        args.push(clip.path.clone());
    }

    let target_res = match opts.resolution.as_deref() {
        Some("480p") => "854:480",
        Some("720p") => "1280:720",
        Some("1440p") => "2560:1440",
        Some("4k") => "3840:2160",
        _ => "1920:1080",
    };

    let mut filter_parts: Vec<String> = Vec::new();
    let mut concat_inputs = String::new();
    for i in 0..n {
        let clip = &timeline[i];
        let has_trim = clip.trim_in > 0.0 || (clip.trim_out > 0.0 && clip.trim_out < 9000.0);
        if has_trim {
            let trim_start = clip.trim_in;
            let trim_end = if clip.trim_out > 0.0 && clip.trim_out < 9000.0 { clip.trim_out } else { 99999.0 };
            filter_parts.push(format!(
                "[{i}:v]trim=start={:.3}:end={:.3},setpts=PTS-STARTPTS,scale={target_res}:force_original_aspect_ratio=decrease,pad={target_res}:(ow-iw)/2:(oh-ih)/2,setsar=1[v{i}]",
                trim_start, trim_end
            ));
            filter_parts.push(format!(
                "[{i}:a]atrim=start={:.3}:end={:.3},asetpts=PTS-STARTPTS,aformat=sample_rates=48000:channel_layouts=stereo[a{i}]",
                trim_start, trim_end
            ));
        } else {
            filter_parts.push(format!(
                "[{i}:v]scale={target_res}:force_original_aspect_ratio=decrease,pad={target_res}:(ow-iw)/2:(oh-ih)/2,setsar=1[v{i}]"
            ));
            filter_parts.push(format!(
                "[{i}:a]aformat=sample_rates=48000:channel_layouts=stereo[a{i}]"
            ));
        }
        concat_inputs.push_str(&format!("[v{i}][a{i}]"));
    }

    let filter_complex = format!(
        "{};{}concat=n={}:v=1:a=1[vout][aout]",
        filter_parts.join(";"),
        concat_inputs,
        n
    );

    args.push("-filter_complex".to_string());
    args.push(filter_complex);
    args.push("-map".to_string());
    args.push("[vout]".to_string());
    args.push("-map".to_string());
    args.push("[aout]".to_string());
    args.push("-r".to_string());
    args.push(format!("{}", opts.fps.unwrap_or(60)));
    args.push("-c:v".to_string());
    args.push("libx264".to_string());
    args.push("-preset".to_string());
    args.push("medium".to_string());
    args.push("-crf".to_string());
    args.push("18".to_string());
    args.push("-c:a".to_string());
    args.push("aac".to_string());
    args.push("-b:a".to_string());
    args.push("192k".to_string());
    args.push(output.to_string());

    let mut child = std::process::Command::new(&ffmpeg_path)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| format!("Failed to run FFmpeg (software): {}", e))?;

    if let Some(stderr) = child.stderr.take() {
        let reader = std::io::BufReader::new(stderr);
        let mut last_pct: u32 = 0;
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if let Some(time_pos) = line.find("time=") {
                let time_str = &line[time_pos + 5..];
                if let Some(end) = time_str.find(|c: char| c == ' ' || c == '\r') {
                    if let Some(secs) = parse_ffmpeg_time(&time_str[..end]) {
                        let pct = ((secs / total_duration) * 100.0).min(99.0) as u32;
                        if pct != last_pct { last_pct = pct; let _ = app.emit("export:progress", pct); }
                    }
                }
            }
        }
    }

    let status = child.wait().map_err(|e| format!("FFmpeg merge error: {}", e))?;
    if !status.success() {
        return Err("Merge failed with software encoder".to_string());
    }
    Ok(())
}

/// Video-only fallback for clips that don't have audio streams.
fn ffmpeg_concat_timeline_video_only(timeline: &[TimelineClip], output: &str, opts: &ExportOpts, total_duration: f64, app: &AppHandle) -> Result<(), String> {
    use std::io::BufRead;
    use std::os::windows::process::CommandExt;

    let ffmpeg_path = find_ffmpeg().ok_or_else(|| "FFmpeg not found".to_string())?;
    let n = timeline.len();
    let mut args: Vec<String> = vec!["-y".to_string()];

    for clip in timeline {
        args.push("-i".to_string());
        args.push(clip.path.clone());
    }

    let target_res = match opts.resolution.as_deref() {
        Some("480p") => "854:480",
        Some("720p") => "1280:720",
        Some("1440p") => "2560:1440",
        Some("4k") => "3840:2160",
        _ => "1920:1080",
    };

    let mut filter_parts: Vec<String> = Vec::new();
    let mut concat_inputs = String::new();
    for i in 0..n {
        let clip = &timeline[i];
        let has_trim = clip.trim_in > 0.0 || (clip.trim_out > 0.0 && clip.trim_out < 9000.0);
        if has_trim {
            let trim_start = clip.trim_in;
            let trim_end = if clip.trim_out > 0.0 && clip.trim_out < 9000.0 { clip.trim_out } else { 99999.0 };
            filter_parts.push(format!(
                "[{i}:v]trim=start={:.3}:end={:.3},setpts=PTS-STARTPTS,scale={target_res}:force_original_aspect_ratio=decrease,pad={target_res}:(ow-iw)/2:(oh-ih)/2,setsar=1[v{i}]",
                trim_start, trim_end
            ));
        } else {
            filter_parts.push(format!(
                "[{i}:v]scale={target_res}:force_original_aspect_ratio=decrease,pad={target_res}:(ow-iw)/2:(oh-ih)/2,setsar=1[v{i}]"
            ));
        }
        concat_inputs.push_str(&format!("[v{i}]"));
    }

    let filter_complex = format!(
        "{};{}concat=n={}:v=1:a=0[vout]",
        filter_parts.join(";"),
        concat_inputs,
        n
    );

    args.push("-filter_complex".to_string());
    args.push(filter_complex);
    args.push("-map".to_string());
    args.push("[vout]".to_string());
    args.push("-r".to_string());
    args.push(format!("{}", opts.fps.unwrap_or(60)));
    args.push("-c:v".to_string());
    args.push("libx264".to_string());
    args.push("-preset".to_string());
    args.push("medium".to_string());
    args.push("-crf".to_string());
    args.push("18".to_string());
    args.push("-an".to_string());
    args.push(output.to_string());

    let mut child = std::process::Command::new(&ffmpeg_path)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| format!("Failed to run FFmpeg (video-only): {}", e))?;

    if let Some(stderr) = child.stderr.take() {
        let reader = std::io::BufReader::new(stderr);
        let mut last_pct: u32 = 0;
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            if let Some(time_pos) = line.find("time=") {
                let time_str = &line[time_pos + 5..];
                if let Some(end) = time_str.find(|c: char| c == ' ' || c == '\r') {
                    if let Some(secs) = parse_ffmpeg_time(&time_str[..end]) {
                        let pct = ((secs / total_duration) * 100.0).min(99.0) as u32;
                        if pct != last_pct { last_pct = pct; let _ = app.emit("export:progress", pct); }
                    }
                }
            }
        }
    }

    let status = child.wait().map_err(|e| format!("FFmpeg merge error: {}", e))?;
    if !status.success() {
        return Err("Merge failed (video-only fallback)".to_string());
    }
    Ok(())
}

/// Run FFmpeg with piped stderr for real-time progress, with proper error handling.
fn ffmpeg_export_piped(app: &AppHandle, input: &str, output: &str, opts: &ExportOpts, effective_duration: f64) -> Result<(), String> {
    use std::io::BufRead;
    use std::os::windows::process::CommandExt;

    // First try NVENC path, building args manually
    let args = build_export_args(input, output, opts, false)?;
    let ffmpeg_path = find_ffmpeg().ok_or_else(|| "FFmpeg not found".to_string())?;

    let mut child = std::process::Command::new(&ffmpeg_path)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x08000000)
        .spawn()
        .map_err(|e| format!("Failed to run FFmpeg: {}", e))?;

    // Parse stderr for progress in real-time
    let mut last_error_line = String::new();
    if let Some(stderr) = child.stderr.take() {
        let reader = std::io::BufReader::new(stderr);
        let mut last_pct: u32 = 0;
        for line in reader.lines() {
            let Ok(line) = line else { continue };
            // Parse time= for progress
            if let Some(time_pos) = line.find("time=") {
                let time_str = &line[time_pos + 5..];
                if let Some(end) = time_str.find(|c: char| c == ' ' || c == '\r') {
                    let time_val = &time_str[..end];
                    if let Some(secs) = parse_ffmpeg_time(time_val) {
                        let pct = if effective_duration > 0.0 {
                            ((secs / effective_duration) * 100.0).min(99.0) as u32
                        } else { 0 };
                        if pct != last_pct {
                            last_pct = pct;
                            let _ = app.emit("export:progress", pct);
                        }
                    }
                }
            }
            // Track error lines
            if line.contains("Error") || line.contains("error") || line.contains("Cannot") || line.contains("Invalid") {
                last_error_line = line.clone();
            }
        }
    }

    let status = child.wait().map_err(|e| format!("FFmpeg error: {}", e))?;

    if !status.success() {
        // If NVENC failed, try software fallback
        if last_error_line.contains("nvenc") || last_error_line.contains("Cannot load") || last_error_line.contains("No NVENC") {
            return ffmpeg_export_software(input, output, opts);
        }
        return Err(format!("Export failed: {}", if last_error_line.is_empty() { "unknown error".to_string() } else { last_error_line }));
    }

    Ok(())
}

/// Build FFmpeg args for export (shared between piped and non-piped paths).
fn build_export_args(input: &str, output: &str, opts: &ExportOpts, software: bool) -> Result<Vec<String>, String> {
    let mut args: Vec<String> = vec!["-y".to_string()];

    // Trim: input seek (will be removed if speed segments are present)
    let mut _has_input_trim = false;
    if let Some(start) = opts.trim_start {
        if start > 0.0 {
            args.push("-ss".to_string());
            args.push(format!("{:.3}", start));
            _has_input_trim = true;
        }
    }

    args.push("-i".to_string());
    args.push(input.to_string());

    if let Some(end) = opts.trim_end {
        let start = opts.trim_start.unwrap_or(0.0);
        let duration = end - start;
        if duration > 0.0 {
            args.push("-t".to_string());
            args.push(format!("{:.3}", duration));
        }
    }

    // Framerate
    args.push("-r".to_string());
    args.push(format!("{}", opts.fps.unwrap_or(60)));

    // Video codec
    if software {
        args.push("-c:v".to_string());
        args.push("libx264".to_string());
        args.push("-preset".to_string());
        args.push("medium".to_string());
        args.push("-crf".to_string());
        args.push("18".to_string());
    } else {
        args.push("-c:v".to_string());
        args.push("h264_nvenc".to_string());
        args.push("-preset".to_string());
        args.push("p7".to_string());
        args.push("-profile:v".to_string());
        args.push("high".to_string());
        args.push("-rc".to_string());
        args.push("vbr".to_string());
        args.push("-cq".to_string());
        args.push("18".to_string());
        args.push("-b:v".to_string());
        args.push("20M".to_string());
        args.push("-maxrate".to_string());
        args.push("30M".to_string());
        args.push("-g".to_string());
        args.push("120".to_string());
    }

    // Resolution / aspect ratio
    let is_vertical = opts.aspect_ratio.as_deref() == Some("9:16") || opts.aspect_ratio.as_deref() == Some("4:5");
    let is_square = opts.aspect_ratio.as_deref() == Some("1:1");

    if is_vertical || is_square {
        let target_h: u32 = match opts.resolution.as_deref() {
            Some("480p") => 854,
            Some("720p") => 1280,
            Some("1080p") => 1920,
            Some("1440p") => 2560,
            _ => 1920,
        };
        let filter = if is_square {
            let side = match opts.resolution.as_deref() {
                Some("480p") => 480, Some("720p") => 720, Some("1080p") => 1080, Some("1440p") => 1440, _ => 1080,
            };
            format!("scale=-2:{},crop={}:{}", side, side, side)
        } else if opts.aspect_ratio.as_deref() == Some("4:5") {
            let target_w = target_h * 4 / 5;
            format!("scale=-2:{},crop={}:{}", target_h, target_w, target_h)
        } else {
            // 9:16
            let target_w = target_h * 9 / 16;
            format!("scale=-2:{},crop={}:{}", target_h, target_w, target_h)
        };
        args.push("-vf".to_string());
        args.push(filter);
    } else {
        if let Some(ref res) = opts.resolution {
            match res.as_str() {
                "480p" => { args.push("-vf".to_string()); args.push("scale=854:480".to_string()); }
                "720p" => { args.push("-vf".to_string()); args.push("scale=1280:720".to_string()); }
                "1080p" => { args.push("-vf".to_string()); args.push("scale=1920:1080".to_string()); }
                "1440p" => { args.push("-vf".to_string()); args.push("scale=2560:1440".to_string()); }
                "4k" => { args.push("-vf".to_string()); args.push("scale=3840:2160".to_string()); }
                _ => {}
            }
        }
    }

    // Video adjustments
    let has_adjustments = opts.brightness.is_some() || opts.contrast.is_some() || opts.saturation.is_some();
    if has_adjustments {
        let b = opts.brightness.unwrap_or(100) as f64 / 100.0;
        let c = opts.contrast.unwrap_or(100) as f64 / 100.0;
        let s = opts.saturation.unwrap_or(100) as f64 / 100.0;
        let eq_filter = format!("eq=brightness={:.2}:contrast={:.2}:saturation={:.2}", b - 1.0, c, s);
        if let Some(pos) = args.iter().position(|a| a == "-vf") {
            let existing = args[pos + 1].clone();
            args[pos + 1] = format!("{},{}", existing, eq_filter);
        } else {
            args.push("-vf".to_string());
            args.push(eq_filter);
        }
    }

    // Speed ramping via filter_complex
    if let Some(ref speed_segs) = opts.speed_segments {
        if !speed_segs.is_empty() {
            let trim_start = opts.trim_start.unwrap_or(0.0);
            let trim_end = opts.trim_end.unwrap_or_else(|| get_video_duration(input).unwrap_or(300.0));

            // Remove -ss and -t — trim is handled inside filter_complex
            if let Some(pos) = args.iter().position(|a| a == "-ss") {
                args.remove(pos + 1);
                args.remove(pos);
            }
            if let Some(pos) = args.iter().position(|a| a == "-t") {
                args.remove(pos + 1);
                args.remove(pos);
            }

            // Speed segments are already in absolute file time (from the video element's currentTime)
            // No offset needed — they already reference the correct positions in the file.
            let adjusted_segs: Vec<SpeedSegmentOpts> = speed_segs.iter().map(|s| SpeedSegmentOpts {
                start: s.start.max(trim_start),
                end: s.end.min(trim_end),
                speed: s.speed,
            }).filter(|s| s.end > s.start).collect();

            let (filter_complex, _, _) = build_speed_filters_with_range(&adjusted_segs, trim_start, trim_end);

            // Remove -vf if present (will be incorporated into filter_complex)
            if let Some(pos) = args.iter().position(|a| a == "-vf") {
                let existing_vf = args[pos + 1].clone();
                args.remove(pos + 1);
                args.remove(pos);
                let full_fc = format!("{};[vout]{}[vfinal]", filter_complex, existing_vf);
                args.push("-filter_complex".to_string());
                args.push(full_fc);
                args.push("-map".to_string());
                args.push("[vfinal]".to_string());
            } else {
                args.push("-filter_complex".to_string());
                args.push(filter_complex);
                args.push("-map".to_string());
                args.push("[vout]".to_string());
            }
            args.push("-map".to_string());
            args.push("[aout]".to_string());
        }
    }

    args.push("-c:a".to_string());
    args.push("aac".to_string());
    args.push("-b:a".to_string());
    args.push("320k".to_string());
    args.push(output.to_string());

    Ok(args)
}

/// Parse FFmpeg time format "HH:MM:SS.xx" to seconds.
fn parse_ffmpeg_time(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    match parts.len() {
        3 => {
            let h: f64 = parts[0].parse().ok()?;
            let m: f64 = parts[1].parse().ok()?;
            let s: f64 = parts[2].parse().ok()?;
            Some(h * 3600.0 + m * 60.0 + s)
        }
        1 => parts[0].parse().ok(),
        _ => None,
    }
}

/// Get video duration using FFmpeg -i (probe).
fn get_video_duration(path: &str) -> Option<f64> {
    use std::os::windows::process::CommandExt;
    let ffmpeg_path = find_ffmpeg()?;
    let output = std::process::Command::new(&ffmpeg_path)
        .args(["-i", path])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x08000000)
        .output()
        .ok()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Look for "Duration: HH:MM:SS.xx"
    if let Some(pos) = stderr.find("Duration: ") {
        let dur_str = &stderr[pos + 10..];
        if let Some(end) = dur_str.find(',') {
            return parse_ffmpeg_time(&dur_str[..end]);
        }
    }
    None
}

/// Build FFmpeg args (shared between progress and non-progress paths).

/// Fallback: software encoder (libx264) if NVENC is unavailable.
/// Uses the same build_export_args logic with software=true, so speed segments work correctly.
fn ffmpeg_export_software(input: &str, output: &str, opts: &ExportOpts) -> Result<(), String> {
    use std::os::windows::process::CommandExt;

    let args = build_export_args(input, output, opts, true)?;
    let ffmpeg_path = find_ffmpeg().ok_or_else(|| "FFmpeg not found".to_string())?;

    let result = std::process::Command::new(&ffmpeg_path)
        .args(&args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x08000000)
        .output()
        .map_err(|e| format!("FFmpeg failed: {}", e))?;

    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        return Err(format!("Export failed: {}", stderr.lines().last().unwrap_or("unknown")));
    }

    Ok(())
}

/// Generate a thumbnail image (JPEG) for a saved clip.
/// Extracts a frame at 1 second into the video. Saves as "{clip_path}.thumb.jpg".
/// Best-effort: failures are logged but don't affect the saved clip.
fn generate_thumbnail(clip_path: &str) {
    use std::os::windows::process::CommandExt;

    let ffmpeg = match find_ffmpeg() {
        Some(f) => f,
        None => {
            eprintln!("[clipsta] Thumbnail: ffmpeg not found, skipping");
            return;
        }
    };

    // Output path: same directory, same name + .thumb.jpg
    let thumb_path = format!("{}.thumb.jpg", clip_path.trim_end_matches(".mp4"));

    let result = std::process::Command::new(&ffmpeg)
        .args([
            "-y",                    // Overwrite
            "-ss", "1",              // Seek to 1 second
            "-i", clip_path,         // Input
            "-vframes", "1",         // Extract 1 frame
            "-vf", "scale=320:-1",   // 320px wide, maintain aspect ratio
            "-q:v", "5",             // JPEG quality (2-31, lower = better)
            &thumb_path,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .output();

    match result {
        Ok(output) if output.status.success() => {
            eprintln!("[clipsta] Thumbnail generated: {}", thumb_path);
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!("[clipsta] Thumbnail failed: {}", stderr.lines().last().unwrap_or("unknown"));
        }
        Err(e) => {
            eprintln!("[clipsta] Thumbnail error: {}", e);
        }
    }
}

/// Find ffmpeg executable on the system
fn find_ffmpeg() -> Option<String> {
    use std::os::windows::process::CommandExt;

    // Check next to our executable first (bundled ffmpeg)
    if let Ok(exe_path) = std::env::current_exe() {
        let dir = exe_path.parent().unwrap_or(std::path::Path::new("."));
        let bundled = dir.join("ffmpeg.exe");
        if bundled.exists() {
            return Some(bundled.to_string_lossy().to_string());
        }
        // Check resources subfolder
        let resources = dir.join("resources").join("ffmpeg.exe");
        if resources.exists() {
            return Some(resources.to_string_lossy().to_string());
        }
    }

    // Check PATH
    if let Ok(output) = std::process::Command::new("where.exe")
        .arg("ffmpeg")
        .creation_flags(0x08000000)
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = path.lines().next() {
                if std::path::Path::new(first_line.trim()).exists() {
                    return Some(first_line.trim().to_string());
                }
            }
        }
    }

    // Check common install locations
    let common_paths = [
        r"C:\Program Files\ffmpeg\bin\ffmpeg.exe",
        r"C:\ffmpeg\bin\ffmpeg.exe",
    ];
    for p in &common_paths {
        if std::path::Path::new(p).exists() {
            return Some(p.to_string());
        }
    }

    None
}

// ── Audio device listing ──────────────────────────────────────────────────────

#[tauri::command]
pub async fn audio_list_devices() -> Result<Vec<serde_json::Value>, String> {
    ensure_com();
    clipsta_capture::audio::WasapiCapture::list_audio_devices().map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn audio_default_devices() -> Result<serde_json::Value, String> {
    ensure_com();
    clipsta_capture::audio::WasapiCapture::get_default_devices().map_err(|e| e.to_string())
}

// ── System info ───────────────────────────────────────────────────────────────

/// Returns the title of the currently focused foreground window.
/// Used for ShadowPlay-style clip naming (e.g., "Battlefield 6").
#[tauri::command]
pub async fn get_active_window_title() -> Result<String, String> {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return Ok("Desktop".to_string());
        }
        let mut buf = [0u16; 512];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len == 0 {
            return Ok("Desktop".to_string());
        }
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        // Strip zero-width and invisible Unicode characters (e.g. Call of Duty uses U+200B
        // between every character in its window title, creating duplicate folder names)
        let title: String = title.chars().filter(|c| {
            !matches!(*c,
                '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{00AD}' |
                '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
            )
        }).collect();
        // Clean up the title — remove common suffixes that aren't game names
        let cleaned = title
            .trim()
            .trim_end_matches(" - Google Chrome")
            .trim_end_matches(" - Mozilla Firefox")
            .trim_end_matches(" - Microsoft Edge")
            .trim_end_matches(" – Mozilla Firefox")
            .trim_end_matches(" - Visual Studio Code")
            .trim_end_matches(" - Discord")
            .to_string();
        // Sanitize for filesystem (remove characters invalid in filenames)
        let safe: String = cleaned.chars()
            .map(|c| match c {
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
                _ => c,
            })
            .collect();
        if safe.is_empty() {
            Ok("Desktop".to_string())
        } else {
            Ok(safe)
        }
    }
}

#[tauri::command]
pub async fn system_info() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "platform": "win32",
        "arch": std::env::consts::ARCH,
        "totalMem": sysinfo_total_mem(),
        "freeMem": sysinfo_free_mem(),
        "cpus": num_cpus(),
    }))
}

// ── Hotkey suspend/resume ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn hotkeys_suspend(app: AppHandle) -> Result<bool, String> {
    app.global_shortcut().unregister_all().map_err(|e| e.to_string())?;
    Ok(true)
}

#[tauri::command]
pub async fn hotkeys_resume(app: AppHandle, store: State<'_, SettingsStore>) -> Result<bool, String> {
    crate::register_hotkeys(&app, &store.get());
    Ok(true)
}

// ── Helper functions ──────────────────────────────────────────────────────────

/// Validate that a path is within the clips output folder (for delete/rename operations).
/// Prevents path traversal attacks where the frontend could delete arbitrary files.
fn validate_clip_path(path: &str, store: &SettingsStore) -> Result<(), String> {
    let settings = store.get();
    let output_folder = if settings.output_folder.is_empty() {
        dirs::video_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("Clipsta")
    } else {
        PathBuf::from(&settings.output_folder)
    };

    let target = std::path::Path::new(path)
        .canonicalize()
        .map_err(|_| "Path does not exist or is invalid".to_string())?;
    let allowed = output_folder
        .canonicalize()
        .unwrap_or(output_folder);

    if !target.starts_with(&allowed) {
        return Err("Access denied: path is outside the clips folder".to_string());
    }
    Ok(())
}

/// Validate that a path is within one of the allowed directories (output folder, videos, downloads, temp).
/// Less restrictive than validate_clip_path — used for read-only operations like file_stat.
fn validate_accessible_path(path: &str, store: &SettingsStore) -> Result<(), String> {
    let settings = store.get();
    let target = std::path::Path::new(path)
        .canonicalize()
        .map_err(|_| "Path does not exist or is invalid".to_string())?;

    let allowed_dirs: Vec<PathBuf> = [
        Some(PathBuf::from(&settings.output_folder)),
        dirs::video_dir(),
        dirs::download_dir(),
        dirs::home_dir().map(|h| h.join("Videos")),
        dirs::home_dir().map(|h| h.join("Desktop")),
        Some(std::env::temp_dir()),
    ]
    .into_iter()
    .flatten()
    .filter(|p| !p.as_os_str().is_empty())
    .collect();

    for dir in &allowed_dirs {
        if let Ok(canonical) = dir.canonicalize() {
            if target.starts_with(&canonical) {
                return Ok(());
            }
        }
        // Also check without canonicalize (dir might not exist yet)
        if target.starts_with(dir) {
            return Ok(());
        }
    }

    Err("Access denied: path is outside allowed directories".to_string())
}

fn ensure_output_folder(store: &SettingsStore) -> PathBuf {
    let settings = store.get();
    let folder = if settings.output_folder.is_empty() {
        let default = dirs::video_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
            .join("Clipsta");
        store.set_field(
            "outputFolder",
            serde_json::Value::String(default.to_string_lossy().to_string()),
        );
        default
    } else {
        PathBuf::from(&settings.output_folder)
    };
    let _ = std::fs::create_dir_all(&folder);
    folder
}

fn unique_path(folder: &std::path::Path, name: &str) -> PathBuf {
    let dest = folder.join(name);
    if !dest.exists() {
        return dest;
    }
    let stem = std::path::Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let ext = std::path::Path::new(name)
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let mut i = 1;
    loop {
        let candidate = folder.join(format!("{} ({}){}", stem, i, if ext.is_empty() { String::new() } else { format!(".{}", ext) }));
        if !candidate.exists() {
            return candidate;
        }
        i += 1;
        // Safety cap: prevent infinite loop if folder is unwritable/full
        if i > 9999 {
            return candidate;
        }
    }
}

/// Resolve bitrate (kbps) based on resolution, fps, and quality preset.
/// Bitrates are tuned to industry standards:
/// - Standard: Efficient, smaller files. Good for sharing/upload.
/// - High: Matches ShadowPlay/ReLive defaults. Best balance of quality and size.
/// - Ultra: Maximum clarity. Matches OBS "Indistinguishable" quality. Large files.
fn resolve_quality_bitrate(resolution: &str, fps: u32, quality: &str) -> u32 {
    let is60 = fps >= 50;
    match quality {
        "standard" => match resolution {
            "480p" => if is60 { 2500 } else { 1500 },
            "720p" => if is60 { 5000 } else { 3000 },
            "1080p" => if is60 { 12000 } else { 8000 },
            "1440p" => if is60 { 30000 } else { 20000 },
            "4k" => if is60 { 50000 } else { 35000 },
            _ => if is60 { 12000 } else { 8000 }, // "native" and unknown → default to 1080p level
        },
        "high" => match resolution {
            "480p" => if is60 { 4000 } else { 2500 },
            "720p" => if is60 { 8000 } else { 5000 },       // Matches ShadowPlay (6.8 Mbps measured)
            "1080p" => if is60 { 20000 } else { 12000 },    // Matches ShadowPlay 1080p
            "1440p" => if is60 { 50000 } else { 30000 },
            "4k" => if is60 { 80000 } else { 50000 },
            _ => if is60 { 20000 } else { 12000 }, // "native" and unknown → default to 1080p level
        },
        "ultra" => match resolution {
            "480p" => if is60 { 8000 } else { 5000 },
            "720p" => if is60 { 15000 } else { 10000 },     // Near-lossless at 720p
            "1080p" => if is60 { 35000 } else { 25000 },    // OBS "Indistinguishable"
            "1440p" => if is60 { 80000 } else { 55000 },
            "4k" => if is60 { 130000 } else { 90000 },
            _ => if is60 { 35000 } else { 25000 }, // "native" and unknown → default to 1080p level
        },
        _ => resolve_quality_bitrate(resolution, fps, "high"), // Unknown → default to high
    }
}

/// Convert a resolution string to (width, height) dimensions.
/// All values are 16-pixel aligned for hardware encoder compatibility.
/// Returns None for "native" — capture uses the source's native dimensions.
fn resolution_to_dimensions(resolution: &str) -> Option<(u32, u32)> {
    // All dimensions MUST be 16-pixel aligned (Clipsta Lite guardrail #2).
    // 1080 → 1088: the extra 8 rows are cropped by players but prevent AMD green
    // macroblock rows and encoder rejection on both NVIDIA and AMD.
    match resolution {
        "native" => None,              // Use captured source dimensions (aligned below)
        "480p" => Some((864, 480)),    // 16-aligned (854 → 864)
        "720p" => Some((1280, 720)),   // Both 16-aligned
        "1080p" => Some((1920, 1088)), // Height 16-aligned (1080 → 1088)
        "1440p" => Some((2560, 1440)), // Both 16-aligned
        "4k" => Some((3840, 2160)),    // Both 16-aligned
        _ => Some((1280, 720)),        // Default to 720p
    }
}







fn sysinfo_total_mem() -> u64 {
    mem_status().map(|s| s.0).unwrap_or(0)
}

fn sysinfo_free_mem() -> u64 {
    mem_status().map(|s| s.1).unwrap_or(0)
}

/// Returns (total_physical, available_physical) in bytes.
fn mem_status() -> Option<(u64, u64)> {
    #[repr(C)]
    #[allow(non_snake_case)]
    struct MEMORYSTATUSEX {
        dwLength: u32,
        dwMemoryLoad: u32,
        ullTotalPhys: u64,
        ullAvailPhys: u64,
        ullTotalPageFile: u64,
        ullAvailPageFile: u64,
        ullTotalVirtual: u64,
        ullAvailVirtual: u64,
        ullAvailExtendedVirtual: u64,
    }
    extern "system" {
        fn GlobalMemoryStatusEx(lpBuffer: *mut MEMORYSTATUSEX) -> i32;
    }
    unsafe {
        let mut status: MEMORYSTATUSEX = std::mem::zeroed();
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut status) != 0 {
            Some((status.ullTotalPhys, status.ullAvailPhys))
        } else {
            None
        }
    }
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

/// Build an FFmpeg filter_complex for segment-based speed ramping.
/// Same as build_speed_filters but with an explicit start/end range.
/// This is used when trim is active — the segments are already in absolute time,
/// and range_start/range_end define the output boundaries.
fn build_speed_filters_with_range(segments: &[SpeedSegmentOpts], range_start: f64, range_end: f64) -> (String, String, bool) {
    if segments.is_empty() {
        // No speed changes, just trim
        let video_filter = format!(
            "[0:v]trim={:.3}:{:.3},setpts=PTS-STARTPTS[vout]",
            range_start, range_end
        );
        let audio_filter = format!(
            "[0:a]atrim={:.3}:{:.3},asetpts=PTS-STARTPTS[aout]",
            range_start, range_end
        );
        return (format!("{};{}", video_filter, audio_filter), String::new(), true);
    }

    // Build ranges within the specified range
    let mut ranges: Vec<(f64, f64, f64)> = Vec::new();
    let mut cursor = range_start;

    let mut sorted_segs = segments.to_vec();
    sorted_segs.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap_or(std::cmp::Ordering::Equal));

    for seg in &sorted_segs {
        let seg_start = seg.start.max(range_start);
        let seg_end = seg.end.min(range_end);
        if seg_start >= seg_end { continue; }

        if seg_start > cursor + 0.01 {
            ranges.push((cursor, seg_start, 1.0));
        }
        ranges.push((seg_start, seg_end, seg.speed));
        cursor = seg_end;
    }
    if cursor < range_end - 0.01 {
        ranges.push((cursor, range_end, 1.0));
    }

    if ranges.is_empty() {
        ranges.push((range_start, range_end, 1.0));
    }

    let n = ranges.len();
    let mut video_parts = Vec::new();
    let mut audio_parts = Vec::new();
    let mut v_labels = Vec::new();
    let mut a_labels = Vec::new();

    for (i, (start, end, speed)) in ranges.iter().enumerate() {
        let inv_speed = 1.0 / speed;
        let vl = format!("v{}", i);
        let al = format!("a{}", i);

        video_parts.push(format!(
            "[0:v]trim={:.3}:{:.3},setpts={:.4}*(PTS-STARTPTS)[{}]",
            start, end, inv_speed, vl
        ));

        let atempo_chain = build_atempo_chain(*speed);
        audio_parts.push(format!(
            "[0:a]atrim={:.3}:{:.3},asetpts=PTS-STARTPTS{}[{}]",
            start, end,
            if atempo_chain.is_empty() { String::new() } else { format!(",{}", atempo_chain) },
            al
        ));

        v_labels.push(format!("[{}]", vl));
        a_labels.push(format!("[{}]", al));
    }

    let video_filter = format!(
        "{};{}concat=n={}:v=1:a=0[vout]",
        video_parts.join(";"),
        v_labels.join(""),
        n
    );
    let audio_filter = format!(
        "{};{}concat=n={}:v=0:a=1[aout]",
        audio_parts.join(";"),
        a_labels.join(""),
        n
    );

    (format!("{};{}", video_filter, audio_filter), String::new(), true)
}

/// Build atempo chain for a given speed (handles range 0.5-100 per filter).
fn build_atempo_chain(speed: f64) -> String {
    if (speed - 1.0).abs() < 0.01 {
        return String::new();
    }
    let mut tempo = speed;
    let mut parts = Vec::new();
    while tempo < 0.5 {
        parts.push("atempo=0.5".to_string());
        tempo /= 0.5;
    }
    while tempo > 100.0 {
        parts.push("atempo=100.0".to_string());
        tempo /= 100.0;
    }
    parts.push(format!("atempo={:.4}", tempo));
    parts.join(",")
}


// ── MP4 Inspection commands ───────────────────────────────────────────────────

#[tauri::command]
pub async fn mp4_inspect(file_path: String) -> Result<crate::mp4_inspect::Mp4Info, String> {
    crate::mp4_inspect::inspect_mp4(&file_path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn mp4_keyframes(file_path: String) -> Result<Vec<f64>, String> {
    let info = crate::mp4_inspect::inspect_mp4(&file_path)
        .await
        .map_err(|e| e.to_string())?;
    Ok(info.keyframes)
}

// ── Lossless Trim command ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn lossless_trim_clip(
    store: State<'_, SettingsStore>,
    input_path: String,
    output_path: String,
    start: f64,
    end: f64,
) -> Result<crate::lossless_trim::TrimResult, String> {
    // Validate both paths are within allowed directories (prevents path traversal)
    validate_accessible_path(&input_path, &store)?;
    validate_accessible_path(&output_path, &store)?;

    // First get keyframes for the input file
    let info = crate::mp4_inspect::inspect_mp4(&input_path)
        .await
        .map_err(|e| format!("Failed to inspect MP4: {}", e))?;

    crate::lossless_trim::lossless_trim(&input_path, &output_path, start, end, &info.keyframes)
        .await
        .map_err(|e| e.to_string())
}

// ── Watch Folder commands ─────────────────────────────────────────────────────

#[tauri::command]
pub async fn watch_folder_start(
    app: AppHandle,
    service: State<'_, crate::watch_folder::WatchFolderService>,
    store: State<'_, SettingsStore>,
) -> Result<bool, String> {
    let settings = store.get();
    let path = settings.watch_folder_path.clone();
    if path.is_empty() {
        return Err("No watch folder path configured".to_string());
    }
    service.start(path, app)?;
    Ok(true)
}

#[tauri::command]
pub async fn watch_folder_stop(
    service: State<'_, crate::watch_folder::WatchFolderService>,
) -> Result<bool, String> {
    service.stop();
    Ok(true)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchFolderStatusResponse {
    pub active: bool,
    pub files_detected: u64,
}

#[tauri::command]
pub async fn watch_folder_status(
    service: State<'_, crate::watch_folder::WatchFolderService>,
) -> Result<WatchFolderStatusResponse, String> {
    Ok(WatchFolderStatusResponse {
        active: service.is_active(),
        files_detected: service.files_detected(),
    })
}

// ── Start at Login ────────────────────────────────────────────────────────────

#[tauri::command]
pub async fn set_start_at_login(enabled: bool) -> Result<bool, String> {
    use std::os::windows::process::CommandExt;

    // In Clipsta 3.0, the capture process is the startup item (not Tauri).
    // It runs as a lightweight tray app with its own hotkeys.
    let exe_dir = std::env::current_exe()
        .map_err(|e| format!("Failed to get exe path: {}", e))?
        .parent()
        .unwrap()
        .to_path_buf();

    // Find clipsta-capture.exe
    let capture_exe = {
        let p1 = exe_dir.join("clipsta-capture.exe");
        if p1.exists() {
            p1
        } else {
            let p2 = exe_dir.join("resources").join("clipsta-capture.exe");
            if p2.exists() {
                p2
            } else {
                // Fallback: register ourselves (legacy behavior)
                std::env::current_exe()
                    .map_err(|e| format!("Failed to get exe path: {}", e))?
            }
        }
    };
    let exe_str = capture_exe.to_string_lossy().to_string();

    if enabled {
        // Add to HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run
        let status = std::process::Command::new("reg")
            .args(["add", r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
                   "/v", "Clipsta", "/t", "REG_SZ", "/d", &format!("\"{}\"", exe_str), "/f"])
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .output()
            .map_err(|e| format!("Registry write failed: {}", e))?;
        if !status.status.success() {
            return Err("Failed to add startup entry".to_string());
        }
    } else {
        // Remove from Run key
        let _ = std::process::Command::new("reg")
            .args(["delete", r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
                   "/v", "Clipsta", "/f"])
            .creation_flags(0x08000000)
            .output();
    }
    Ok(enabled)
}
