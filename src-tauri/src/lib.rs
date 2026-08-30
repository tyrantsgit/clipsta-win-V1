//! Clipsta Tauri v2 — main application setup
//!
//! - All commands registered
//! - System tray with context menu
//! - Global shortcuts for clip saves and recording toggle
//! - In-process WGC + WASAPI capture (no separate process)

pub mod capture_proxy;
pub mod cloud_proxy;
pub mod commands;
pub mod lossless_trim;
pub mod mp4_inspect;
pub mod settings;
pub mod watch_folder;


use std::sync::atomic::Ordering;
use std::sync::mpsc as std_mpsc;
use std::sync::Arc;

use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::TrayIconBuilder;
use tauri::{Emitter, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut};

use settings::SettingsStore;
use watch_folder::WatchFolderService;

/// Channel sender for hotkey-triggered clip saves.
/// Lives as a global so the hotkey closure (which can't access Tauri state) can send save requests.
static SAVE_TX: std::sync::OnceLock<std_mpsc::SyncSender<u32>> = std::sync::OnceLock::new();

/// Register global hotkeys based on current settings.
pub fn register_hotkeys(app: &tauri::AppHandle, settings: &settings::AppSettings) {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();

    let app_clone = app.clone();
    let try_register = |accel: &str, seconds: u32| {
        if accel.is_empty() {
            return;
        }
        if let Ok(shortcut) = accel.parse::<Shortcut>() {
            let _ = gs.on_shortcut(shortcut, move |_app, _shortcut, event| {
                // Only fire on key-down (Pressed), not key-up (Released).
                // Without this check, each hotkey press creates 2 clips.
                if event.state() != tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    return;
                }
                // Send save request through the global channel.
                if let Some(tx) = SAVE_TX.get() {
                    let _ = tx.try_send(seconds);
                }
            });
        }
    };

    try_register(&settings.hotkey_clip30_sec, 30);
    try_register(&settings.hotkey_clip1_min, 60);
    try_register(&settings.hotkey_clip5_min, 300);

    // Record toggle (still emits to WebView — only used when app is in foreground for UI)
    if !settings.hotkey_record.is_empty() {
        if let Ok(shortcut) = settings.hotkey_record.parse::<Shortcut>() {
            let app_h = app_clone.clone();
            let _ = gs.on_shortcut(shortcut, move |_app, _shortcut, event| {
                // Same guardrail as the clip-save hotkeys above: fire once per
                // press, not once per press AND once per release.
                if event.state() != tauri_plugin_global_shortcut::ShortcutState::Pressed {
                    return;
                }
                let _ = app_h.emit("hotkey:record", ());
            });
        }
    }
}

/// Find the existing Clipsta window and bring it to the foreground.
/// Best-effort: if we can't find it, the user will see nothing happen (graceful).
fn bring_existing_window_to_front() {
    unsafe {
        use windows::Win32::Foundation::{HWND, LPARAM};
        use windows::Win32::UI::WindowsAndMessaging::*;
        use windows_core::BOOL;

        // EnumWindows callback: find a window with title containing "Clipsta"
        unsafe extern "system" fn enum_callback(hwnd: HWND, _: LPARAM) -> BOOL {
            let mut buf = [0u16; 256];
            let len = GetWindowTextW(hwnd, &mut buf);
            if len > 0 {
                let title = String::from_utf16_lossy(&buf[..len as usize]);
                if title.contains("Clipsta") {
                    // Found it — restore if minimized and bring to front
                    if IsIconic(hwnd).as_bool() {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(hwnd);
                    return BOOL(0); // Stop enumerating
                }
            }
            BOOL(1) // Continue
        }

        let _ = EnumWindows(Some(enum_callback), LPARAM(0));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // ── Single Instance Guard ─────────────────────────────────────────────────
    // Prevent multiple instances from fighting over the named pipe and hotkeys.
    // Uses a named mutex — if it already exists, another instance is running.
    let _instance_mutex = unsafe {
        use windows::Win32::Foundation::GetLastError;
        use windows::Win32::System::Threading::CreateMutexW;
        use windows_core::PCWSTR;

        let name: Vec<u16> = "Global\\ClipstaDesktopV2"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let handle = CreateMutexW(None, true, PCWSTR(name.as_ptr()));

        match handle {
            Ok(h) => {
                // Check if mutex already existed (another instance owns it)
                if GetLastError().0 == 183 {
                    // ERROR_ALREADY_EXISTS = 183
                    // Another instance is running — try to bring its window to front
                    eprintln!("[clipsta] Another instance is already running. Activating it.");
                    bring_existing_window_to_front();
                    std::process::exit(0);
                }
                Some(h)
            }
            Err(_) => {
                // Mutex creation failed — proceed anyway (better than blocking launch)
                eprintln!("[clipsta] Warning: Could not create instance mutex");
                None
            }
        }
    };

    // NOTE: Do NOT call CoInitializeEx here. Tauri's window library (tao) requires
    // OleInitialize (STA) on the main thread. COM MTA is initialized on capture/audio
    // threads instead (see capture.rs and audio.rs).

    // Disable WebView2 GPU compositing to prevent D3D11/Dawn crashes during gameplay.
    // The crash was specifically in Dawn's BeginRenderPassCmd (the compositing step).
    // This keeps GPU rasterization for the UI but uses software for final frame assembly.
    // Disable ALL WebView2 GPU usage to prevent crashes during gameplay.
    // WebView2's renderer (both Dawn compositor and Skia rasterizer) crashes under
    // heavy GPU load from games + capture encoder. CPU rendering for the HTML UI is
    // fast enough — it's just buttons, text, and thumbnails.
    std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
        "--disable-gpu --disk-cache-size=10485760 --disable-background-networking --disable-background-timer-throttling --disable-backgrounding-occluded-windows --disable-renderer-backgrounding --js-flags=--max-old-space-size=128 --renderer-process-limit=2 --disable-features=V8IdleTasks --disable-dev-shm-usage --single-process --autoplay-policy=no-user-gesture-required");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            // Settings
            commands::settings_get_all,
            commands::settings_set,
            commands::settings_set_all,
            // Clips
            commands::clips_list,
            commands::clips_delete,
            commands::clips_rename,
            commands::clips_import,
            commands::clips_import_folder,
            // Recording
            commands::wgc_sources,
            commands::wgc_capture_diagnostics,
            commands::wgc_start_recording,
            commands::wgc_stop_recording,
            commands::wgc_save_clip,
            commands::wgc_save_full_recording,
            // Export
            commands::recording_export,
            commands::compress_for_upload,
            commands::native_upload_clip,
            // File ops
            commands::shell_open_folder,
            commands::shell_open_file,
            commands::shell_show_item,
            commands::file_stat,
            commands::file_ensure_dir,
            commands::file_copy_to_downloads,
            // Audio
            commands::audio_list_devices,
            commands::audio_default_devices,
            // System
            commands::system_info,
            commands::get_active_window_title,
            // Hotkeys
            commands::hotkeys_suspend,
            commands::hotkeys_resume,
            // MP4 Inspection
            commands::mp4_inspect,
            commands::mp4_keyframes,
            // Lossless Trim
            commands::lossless_trim_clip,
            // Watch Folder
            commands::watch_folder_start,
            commands::watch_folder_stop,
            commands::watch_folder_status,
            commands::set_start_at_login,
            // Cloud Proxy (API key stays backend-side)
            cloud_proxy::cloud_get_config,
            cloud_proxy::cloud_generate_pairing,
            cloud_proxy::cloud_request_upload,
            cloud_proxy::cloud_notify_status,
        ])
        .setup(|app| {
            // Load settings (non-fatal: use defaults if file is corrupted)
            let app_data = app
                .path()
                .app_data_dir()
                .unwrap_or_else(|_| std::path::PathBuf::from("."));
            let _ = std::fs::create_dir_all(&app_data);
            let store = SettingsStore::load(&app_data);

            // Migrate output folder: v2.3.0 defaulted to Downloads/Clipsta,
            // v2.3.1+ uses Videos/Clipsta. Migrate existing users automatically.
            {
                let settings = store.get();
                let downloads_clipsta = dirs::download_dir()
                    .unwrap_or_default()
                    .join("Clipsta")
                    .to_string_lossy()
                    .to_string();
                if settings.output_folder == downloads_clipsta {
                    let videos_clipsta = dirs::video_dir()
                        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from(".")))
                        .join("Clipsta")
                        .to_string_lossy()
                        .to_string();
                    store.set_field("outputFolder", serde_json::Value::String(videos_clipsta));
                }
            }

            // Ensure output folder exists
            let settings = store.get();
            if !settings.output_folder.is_empty() {
                let _ = std::fs::create_dir_all(&settings.output_folder);
            }

            // Create capture session
            let proxy = match capture_proxy::CaptureProxy::spawn_and_connect(app.handle().clone()) {
                Ok(p) => {
                    eprintln!("[clipsta] Capture process connected successfully");
                    Arc::new(p)
                }
                Err(e) => {
                    eprintln!("[clipsta] FATAL: Failed to spawn capture process: {}", e);
                    // Fall back to showing error — can't function without capture
                    panic!("Cannot start without capture process: {}", e);
                }
            };

            // Clean up old temp recording directories and orphaned temp files from previous sessions
            let temp_dir = std::env::temp_dir();
            if let Ok(entries) = std::fs::read_dir(&temp_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    // Remove orphaned segment directories
                    if name.starts_with("clipsta_segments_") || name == "clipsta_recording" {
                        let _ = std::fs::remove_dir_all(entry.path());
                    }
                    // Remove orphaned clip temp files (from crashed saves)
                    if name.starts_with("clipsta_clip_video_") || name.starts_with("clipsta_concat_") {
                        let _ = std::fs::remove_file(entry.path());
                    }
                    // Remove orphaned mmap ring buffer file (from hard crash of capture process)
                    if name == "clipsta_ring_video.bin" {
                        let _ = std::fs::remove_file(entry.path());
                    }
                    // Remove old debug log files
                    if name.starts_with("clipsta_") && (name.ends_with(".log") || name.ends_with(".txt")) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }

            // Manage state
            app.manage(store.clone());
            app.manage(proxy.clone());
            app.manage(cloud_proxy::HttpClient::new());

            // Spawn the save-worker thread: receives hotkey save requests via channel,
            // sends SAVE commands to the capture process via IPC.
            // This is the Clipsta Lite architecture — hotkeys never touch the UI.
            {
                let (tx, rx) = std_mpsc::sync_channel::<u32>(4);
                let _ = SAVE_TX.set(tx);

                let is_recording = proxy.is_recording.clone();
                let is_saving = proxy.is_saving.clone();
                let store_for_worker = store.clone();
                let proxy_for_save = proxy.clone();
                let app_for_worker = app.handle().clone();

                std::thread::Builder::new()
                    .name("clipsta-save-worker".into())
                    .spawn(move || {
                        eprintln!("[clipsta] Save-worker thread started (IPC mode)");

                        while let Ok(seconds) = rx.recv() {
                            if !is_recording.load(Ordering::Relaxed) {
                                continue;
                            }
                            if is_saving.load(Ordering::Relaxed) {
                                continue;
                            }

                            let settings = store_for_worker.get();

                            // Generate filename (ShadowPlay style)
                            let now = chrono::Local::now();
                            let cs = now.format("%f").to_string();
                            let centiseconds = &cs[..2.min(cs.len())];
                            let stamp = format!("{}.{}", now.format("%Y.%m.%d - %H.%M.%S"), centiseconds);
                            let game_name = unsafe {
                                use windows::Win32::UI::WindowsAndMessaging::*;
                                let hwnd = GetForegroundWindow();
                                let mut buf = [0u16; 256];
                                let len = GetWindowTextW(hwnd, &mut buf);
                                if len > 0 {
                                    let title = String::from_utf16_lossy(&buf[..len as usize]);
                                    // Strip zero-width and invisible Unicode characters
                                    let title: String = title.chars().filter(|c| {
                                        !matches!(*c,
                                            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{00AD}' |
                                            '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}'
                                        )
                                    }).collect();
                                    // Clean up browser/app suffixes
                                    let cleaned = title.trim()
                                        .trim_end_matches(" - Google Chrome")
                                        .trim_end_matches(" - Mozilla Firefox")
                                        .trim_end_matches(" - Microsoft Edge")
                                        .trim_end_matches(" - Visual Studio Code")
                                        .trim_end_matches(" - Discord")
                                        .to_string();
                                    // Sanitize for filesystem
                                    cleaned.chars()
                                        .map(|c| match c { '<'|'>'|':'|'"'|'/'|'\\'|'|'|'?'|'*' => '_', _ => c })
                                        .collect::<String>()
                                } else {
                                    "Desktop".to_string()
                                }
                            };

                            let file_name = format!("{} {}.DVR.mp4", game_name, stamp);
                            let output_folder = std::path::PathBuf::from(&settings.output_folder);
                            let _ = std::fs::create_dir_all(&output_folder);

                            // Only create game subfolder for actual games (not Desktop, not browser titles)
                            let is_desktop = game_name.is_empty()
                                || game_name == "Desktop"
                                || game_name == "Clipsta"
                                || game_name.contains(" - ");
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
                            let result = proxy_for_save.save_clip(seconds, &output_str);
                            match result {
                                Ok(ref path) => {
                                    eprintln!("[clipsta] Clip saved: {}", path);
                                    // Emit events to frontend
                                    let _ = app_for_worker.emit("wgc:clipSaved", path);
                                    if settings.clip_sound_enabled {
                                        let _ = app_for_worker.emit("play-clip-sound", ());
                                    }

                                    // Auto-upload in Rust (no WebView involvement).
                                    let upload_settings = store_for_worker.get();
                                    if upload_settings.auto_upload && upload_settings.cloud_enabled && !upload_settings.cloud_pair_code.is_empty() {
                                        let path_clone = path.clone();
                                        let device_id = upload_settings.desktop_device_id.clone();
                                        std::thread::spawn(move || {
                                            if let Err(e) = do_rust_upload(&path_clone, &device_id) {
                                                eprintln!("[clipsta] Auto-upload failed: {}", e);
                                            }
                                        });
                                    }
                                }
                                Err(e) => {
                                    if !e.contains("No keyframe") && !e.contains("Not recording") && !e.contains("Save already") {
                                        eprintln!("[clipsta] Clip save error: {}", e);
                                    }
                                }
                            }
                        }
                    })
                    .expect("Failed to spawn save-worker thread");
            }

            // Create and manage watch folder service
            let watch_service = WatchFolderService::new();
            app.manage(watch_service.clone());

            // Auto-start watch folder if enabled in settings
            if settings.watch_folder_enabled && !settings.watch_folder_path.is_empty() {
                let app_handle = app.handle().clone();
                let path = settings.watch_folder_path.clone();
                let svc = watch_service.clone();
                // Start on a background task (needs tokio runtime)
                tokio::spawn(async move {
                    if let Err(e) = svc.start(path.clone(), app_handle) {
                        eprintln!("[watch_folder] auto-start failed: {}", e);
                    }
                });
            }

            // Setup tray icon (non-fatal if it fails)
            if let Err(e) = setup_tray(app) {
                eprintln!("[clipsta] tray setup failed (non-fatal): {}", e);
            }

            // Register global shortcuts (non-fatal: app works without them)
            let app_handle = app.handle().clone();
            register_hotkeys(&app_handle, &settings);

            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let app = window.app_handle();
                let store = app.state::<SettingsStore>();
                if store.get().minimize_to_tray {
                    // Hide to tray — recording continues in the capture process.
                    api.prevent_close();
                    let _ = window.hide();
                } else {
                    // Disconnect from capture process (don't kill it — it's independent)
                    let proxy = app.state::<Arc<capture_proxy::CaptureProxy>>();
                    proxy.shutdown();
                    app.exit(0);
                }
            }
        })
        .run(tauri::generate_context!())
        .unwrap_or_else(|e| {
            // Show a native error dialog so users see something instead of a silent crash
            let msg = format!(
                "Clipsta failed to start:\n{}\n\n\
                Possible fixes:\n\
                • Install Microsoft Edge WebView2 Runtime\n\
                • Update your GPU drivers\n\
                • Restart your PC",
                e
            );
            eprintln!("{}", msg);
            // Use Windows MessageBox for fatal errors (no Tauri window available)
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStrExt;
                let wide_msg: Vec<u16> = std::ffi::OsStr::new(&msg).encode_wide().chain(std::iter::once(0)).collect();
                let wide_title: Vec<u16> = std::ffi::OsStr::new("Clipsta - Startup Error").encode_wide().chain(std::iter::once(0)).collect();
                unsafe {
                    windows::Win32::UI::WindowsAndMessaging::MessageBoxW(
                        None,
                        windows::core::PCWSTR(wide_msg.as_ptr()),
                        windows::core::PCWSTR(wide_title.as_ptr()),
                        windows::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
                    );
                }
            }
            std::process::exit(1);
        });
}

fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let open_item = MenuItemBuilder::with_id("open", "Open Clipsta").build(app)?;
    let clip30_item = MenuItemBuilder::with_id("clip30", "⚡ Save Last 30 Seconds").build(app)?;
    let clip1m_item = MenuItemBuilder::with_id("clip1m", "⚡ Save Last 1 Minute").build(app)?;
    let clip5m_item = MenuItemBuilder::with_id("clip5m", "⚡ Save Last 5 Minutes").build(app)?;
    let folder_item = MenuItemBuilder::with_id("folder", "Open Clips Folder").build(app)?;
    let quit_item = MenuItemBuilder::with_id("quit", "Quit Clipsta").build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&open_item)
        .separator()
        .item(&clip30_item)
        .item(&clip1m_item)
        .item(&clip5m_item)
        .separator()
        .item(&folder_item)
        .separator()
        .item(&quit_item)
        .build()?;

    let _tray = TrayIconBuilder::new()
        .tooltip("Clipsta")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "clip30" => {
                if let Some(tx) = SAVE_TX.get() { let _ = tx.try_send(30); }
            }
            "clip1m" => {
                if let Some(tx) = SAVE_TX.get() { let _ = tx.try_send(60); }
            }
            "clip5m" => {
                if let Some(tx) = SAVE_TX.get() { let _ = tx.try_send(300); }
            }
            "folder" => {
                let store = app.state::<SettingsStore>();
                let folder = store.get().output_folder;
                if !folder.is_empty() {
                    let _ = std::process::Command::new("explorer").arg(&folder).spawn();
                }
            }
            "quit" => {
                // Disconnect from capture process (it keeps running independently)
                let proxy = app.state::<Arc<capture_proxy::CaptureProxy>>();
                proxy.shutdown();
                app.exit(0);
            }
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::DoubleClick { .. } = event {
                let app = tray.app_handle();
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
        })
        .build(app)?;

    Ok(())
}

/// A `Read` wrapper that reports bytes-read progress via a callback.
///
/// Wrapping the file handle (instead of `std::fs::read` into a `Vec`) gives us
/// two wins at once: (1) we don't pull 100 MB+ into RAM, and (2) reqwest pulls
/// bytes through this reader as it writes the socket, so `on_progress` fires with
/// the *actual* number of bytes handed to the transport — real upload progress.
struct ProgressReader<R: std::io::Read, F: FnMut(u64)> {
    inner: R,
    read_so_far: u64,
    on_progress: F,
}

impl<R: std::io::Read, F: FnMut(u64)> std::io::Read for ProgressReader<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        if n > 0 {
            self.read_so_far += n as u64;
            (self.on_progress)(self.read_so_far);
        }
        Ok(n)
    }
}

/// Classified upload error so retry logic can be structured instead of string-matching.
struct UploadError {
    message: String,
    /// True for network/timeout errors, HTTP 5xx and HTTP 429 — worth retrying.
    /// False for other 4xx (auth/validation) and local errors — permanent.
    transient: bool,
}

impl UploadError {
    fn transient(msg: impl Into<String>) -> Self {
        Self { message: msg.into(), transient: true }
    }
    fn permanent(msg: impl Into<String>) -> Self {
        Self { message: msg.into(), transient: false }
    }
    /// Classify an HTTP status: 5xx and 429 are transient, other non-2xx are permanent.
    fn from_status(prefix: &str, status: u16) -> Self {
        let msg = format!("{}: HTTP {}", prefix, status);
        if status >= 500 || status == 429 {
            Self::transient(msg)
        } else {
            Self::permanent(msg)
        }
    }
}

/// Upload a clip directly from Rust (no WebView involvement).
///
/// Back-compat entry point used by the hotkey auto-upload path (no progress UI).
/// Delegates to `do_rust_upload_ex` with no AppHandle and the legacy 30s duration.
fn do_rust_upload(file_path: &str, device_id: &str) -> Result<(), String> {
    do_rust_upload_ex(file_path, device_id, None, None)
}

/// Upload a clip directly from Rust with optional real-time progress events.
///
/// Enhancement over the original whole-file upload:
///   * Streams the file through a `ProgressReader` and emits `upload:progress`
///     Tauri events (`{ id, sent, total, percent }`) as bytes go out the socket.
///   * No total request timeout (only connect + per-read timeouts) so 100 MB+
///     clips are never aborted mid-transfer.
///   * Bounded retry with exponential backoff for transient failures (network
///     errors, HTTP 5xx, HTTP 429); other 4xx are permanent.
///   * `duration_seconds` uses the real clip duration when known (falls back to 30).
///
/// The wire format is IDENTICAL to the original: a multipart POST with a `file`
/// part named after the clip and `video/mp4` MIME. This is the guaranteed
/// fallback — nothing about auth, endpoints, or JSON shapes changes.
///
/// RESUMABLE NOTE: the `/clip-uploads` endpoint hands back a Cloudflare Stream
/// *direct-upload* URL, which expects a single multipart POST and does NOT
/// advertise tus/Range support. Byte-level resume would require a separate tus
/// endpoint that we must not assume exists. Retries therefore re-send the whole
/// file (the progress bar simply restarts), which is the correct graceful
/// degradation for a direct-upload URL.
fn do_rust_upload_ex(
    file_path: &str,
    device_id: &str,
    app: Option<&tauri::AppHandle>,
    duration_seconds: Option<u32>,
) -> Result<(), String> {
    use crate::cloud_proxy::{cloud_api_key, CLOUD_API_BASE};

    let path = std::path::Path::new(file_path);
    let file_name = path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let file_size = std::fs::metadata(file_path)
        .map(|m| m.len())
        .unwrap_or(0);

    if file_size == 0 {
        return Err("File is empty or doesn't exist".to_string());
    }

    eprintln!("[clipsta] Auto-uploading: {} ({} MB)", file_name, file_size / (1024 * 1024));

    // HTTP client for the transfer: NO total timeout (large clips must not abort),
    // only a connect timeout so we still fail fast if the endpoint is unreachable.
    // A dead socket mid-transfer eventually surfaces as a network error, which the
    // retry logic below classifies as transient and re-attempts.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    let bounded_retries: u32 = 4;
    let mut attempt: u32 = 0;

    loop {
        attempt += 1;
        match upload_attempt(
            &client,
            CLOUD_API_BASE,
            cloud_api_key(),
            file_path,
            &file_name,
            device_id,
            file_size,
            duration_seconds.unwrap_or(30),
            app,
        ) {
            Ok(()) => {
                eprintln!("[clipsta] Auto-upload complete: {}", file_name);
                if let Some(app) = app {
                    let _ = app.emit("upload:progress", serde_json::json!({
                        "id": file_path,
                        "sent": file_size,
                        "total": file_size,
                        "percent": 100u32,
                    }));
                }
                return Ok(());
            }
            Err(e) => {
                if e.transient && attempt <= bounded_retries {
                    // Exponential backoff: 1s, 2s, 4s, 8s (capped).
                    let backoff_ms = 1000u64 * (1u64 << (attempt - 1).min(3));
                    eprintln!(
                        "[clipsta] Upload attempt {} failed (transient): {} — retrying in {}ms",
                        attempt, e.message, backoff_ms
                    );
                    if let Some(app) = app {
                        let _ = app.emit("upload:retry", serde_json::json!({
                            "id": file_path,
                            "attempt": attempt,
                            "maxAttempts": bounded_retries,
                            "message": e.message,
                        }));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(backoff_ms));
                    continue;
                }
                eprintln!("[clipsta] Upload failed permanently: {}", e.message);
                return Err(e.message);
            }
        }
    }
}

/// A single upload attempt: request a fresh upload URL, then stream the file.
/// A fresh URL is requested per attempt because direct-upload URLs are single-use.
#[allow(clippy::too_many_arguments)]
fn upload_attempt(
    client: &reqwest::blocking::Client,
    api_base: &str,
    api_key: &str,
    file_path: &str,
    file_name: &str,
    device_id: &str,
    file_size: u64,
    duration_seconds: u32,
    app: Option<&tauri::AppHandle>,
) -> Result<(), UploadError> {
    // Step 1: Request upload URL from cloud API (unchanged JSON shape).
    let upload_req = serde_json::json!({
        "desktopDeviceId": device_id,
        "fileName": file_name,
        "durationSeconds": duration_seconds,
        "bytes": file_size,
        "capturedAt": chrono::Local::now().to_rfc3339(),
    });

    let res = client
        .post(format!("{}/clip-uploads", api_base))
        .header("Content-Type", "application/json")
        .header("X-Clipsta-Test-Key", api_key)
        .json(&upload_req)
        .send()
        .map_err(|e| UploadError::transient(format!("Upload request failed: {}", e)))?;

    if !res.status().is_success() {
        return Err(UploadError::from_status("Cloud API error", res.status().as_u16()));
    }

    let data: serde_json::Value = res.json()
        .map_err(|e| UploadError::permanent(format!("Parse error: {}", e)))?;

    let upload_url = data["uploadUrl"].as_str()
        .ok_or_else(|| UploadError::permanent("No uploadUrl in response"))?
        .to_string();

    // Step 2: Stream the file as a multipart form (SAME wire format as before),
    // wrapping the file handle in a ProgressReader so we can emit real progress.
    let file = std::fs::File::open(file_path)
        .map_err(|e| UploadError::permanent(format!("Read file failed: {}", e)))?;

    // Throttle progress emits: at most one event per ~1% or per ~1MB, whichever
    // comes first, so we don't flood the event bus for large files.
    let mut last_percent: i64 = -1;
    let file_path_owned = file_path.to_string();
    let app_cloned = app.cloned();
    let progress_reader = ProgressReader {
        inner: file,
        read_so_far: 0,
        on_progress: move |sent| {
            if let Some(app) = &app_cloned {
                let percent = if file_size > 0 {
                    ((sent as f64 / file_size as f64) * 100.0) as i64
                } else {
                    0
                };
                if percent != last_percent {
                    last_percent = percent;
                    let _ = app.emit("upload:progress", serde_json::json!({
                        "id": file_path_owned,
                        "sent": sent,
                        "total": file_size,
                        "percent": percent.clamp(0, 100) as u32,
                    }));
                }
            }
        },
    };

    let part = reqwest::blocking::multipart::Part::reader_with_length(progress_reader, file_size)
        .file_name(file_name.to_string())
        .mime_str("video/mp4")
        .map_err(|e| UploadError::permanent(format!("Multipart error: {}", e)))?;

    let form = reqwest::blocking::multipart::Form::new()
        .part("file", part);

    let upload_res = client
        .post(&upload_url)
        .multipart(form)
        .send()
        .map_err(|e| UploadError::transient(format!("Upload failed: {}", e)))?;

    if !upload_res.status().is_success() {
        return Err(UploadError::from_status("Upload", upload_res.status().as_u16()));
    }

    Ok(())
}
