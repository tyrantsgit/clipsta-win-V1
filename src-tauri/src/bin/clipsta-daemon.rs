//! clipsta-daemon.exe — ShadowPlay-style always-on capture daemon.
//!
//! Runs independently of the Tauri UI:
//! - Auto-starts WGC capture on launch
//! - Registers global hotkeys (Ctrl+Shift+G = 30s, Alt+F9 = 60s, Alt+F10 = 5min)
//! - Saves clips directly to the output folder
//! - Plays chime on save
//! - Exposes named pipe for Tauri UI to query status
//! - System tray icon with status indicator
//!
//! The Tauri app connects to this daemon for settings changes and status,
//! but the daemon operates fully independently (like NVIDIA ShadowPlay).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use clipsta_capture::gpu_capture::{CaptureOptions, CaptureSession};
use clipsta_capture::chime;
use clipsta_tauri_lib::settings::{AppSettings, SettingsStore};

/// Daemon state shared across threads.
struct DaemonState {
    session: CaptureSession,
    settings: SettingsStore,
    running: Arc<AtomicBool>,
}

fn main() {
    // Initialize COM + Media Foundation
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL};
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let _ = MFStartup(0x0002_0070, MFSTARTUP_FULL);
    }

    eprintln!("[clipsta-daemon] Starting (ShadowPlay mode)");
    eprintln!("[clipsta-daemon] PID {}", std::process::id());

    // Load settings from the shared settings file
    let app_data = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gg.clipsta.desktop");
    let settings_store = SettingsStore::load(&app_data);
    let settings = settings_store.get();

    eprintln!("[clipsta-daemon] Output folder: {}", settings.output_folder);
    eprintln!("[clipsta-daemon] Buffer: {}s, FPS: {}, Mic: {}",
        settings.buffer_duration, settings.fps, settings.capture_mic);

    // Create capture session
    let session = CaptureSession::new();
    session.warm_start();

    let running = Arc::new(AtomicBool::new(true));

    // Start capture immediately (ShadowPlay behavior)
    let capture_started = start_capture(&session, &settings);
    if !capture_started {
        eprintln!("[clipsta-daemon] FATAL: Failed to start capture");
        std::process::exit(1);
    }

    eprintln!("[clipsta-daemon] Capture active — listening for hotkeys");

    // Register global hotkeys
    let running_hotkey = running.clone();
    let hotkey_thread = thread::spawn(move || {
        run_hotkey_loop(running_hotkey);
    });

    // Main loop: listen for hotkey events and pipe commands
    let session_arc = Arc::new(session);
    let settings_arc = Arc::new(settings_store);

    // Hotkey receiver channel
    let (save_tx, save_rx) = std::sync::mpsc::channel::<u32>();

    // Register hotkeys using Windows API
    register_global_hotkeys(&settings, save_tx.clone());

    // Also start named pipe server for Tauri UI communication
    let session_pipe = session_arc.clone();
    let settings_pipe = settings_arc.clone();
    let running_pipe = running.clone();
    let save_tx_pipe = save_tx.clone();
    thread::spawn(move || {
        run_pipe_server(session_pipe, settings_pipe, running_pipe, save_tx_pipe);
    });

    // Main save loop
    while running.load(Ordering::Relaxed) {
        match save_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(seconds) => {
                let s = settings_arc.get();
                save_clip(&session_arc, &s, seconds);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    // Cleanup
    session_arc.stop();
    session_arc.cleanup();
    eprintln!("[clipsta-daemon] Shutdown complete");
}

fn start_capture(session: &CaptureSession, settings: &AppSettings) -> bool {
    let seg_dir = std::env::temp_dir().join("clipsta_daemon");
    if seg_dir.exists() {
        let _ = std::fs::remove_dir_all(&seg_dir);
    }

    // Resolve dimensions from settings
    let (target_w, target_h) = match settings.resolution.as_str() {
        "720p" => (Some(1280), Some(720)),
        "1080p" => (Some(1920), Some(1088)),
        "1440p" => (Some(2560), Some(1440)),
        _ => (None, None),
    };

    let bitrate = match settings.quality.as_str() {
        "low" => 4000,
        "medium" => 8000,
        "high" => 15000,
        "ultra" => 25000,
        _ => 8000,
    };

    let mic_device = if settings.capture_mic {
        if settings.audio_input_device_id.is_empty() {
            Some("default".to_string())
        } else {
            Some(settings.audio_input_device_id.clone())
        }
    } else {
        None
    };

    let loopback = if settings.desktop_audio_device_id.is_empty() {
        None
    } else {
        Some(settings.desktop_audio_device_id.clone())
    };

    let opts = CaptureOptions {
        source_id: None,
        fps: settings.fps,
        no_audio: !settings.capture_audio,
        mic_device,
        loopback_device: loopback,
        target_width: target_w,
        target_height: target_h,
        bitrate_kbps: bitrate,
        segment_duration: 3,
        buffer_duration: settings.buffer_duration,
        segment_dir: seg_dir,
        multi_track_audio: settings.multi_track_audio,
        warm_cache: Some(session.warm_cache.clone()),
    };

    let on_segment = Box::new(|_seg: clipsta_capture::gpu_capture::CompletedSegment| {});
    let on_died = None;

    match session.start(opts, on_segment, on_died) {
        Ok(_info) => {
            eprintln!("[clipsta-daemon] Capture started successfully");
            true
        }
        Err(e) => {
            eprintln!("[clipsta-daemon] Capture start failed: {}", e);
            false
        }
    }
}

fn save_clip(session: &CaptureSession, settings: &AppSettings, seconds: u32) {
    if !session.is_recording.load(Ordering::Relaxed) {
        eprintln!("[clipsta-daemon] Cannot save — not recording");
        return;
    }
    if session.is_saving.load(Ordering::Relaxed) {
        eprintln!("[clipsta-daemon] Save already in progress");
        return;
    }

    // Generate ShadowPlay-style filename
    let now = chrono::Local::now();
    let stamp = now.format("%Y.%m.%d - %H.%M.%S.%2f");

    // Get active window title for game name
    let game_name = get_active_window_title().unwrap_or_else(|| "Desktop".to_string());

    let output_folder = PathBuf::from(&settings.output_folder);
    let game_folder = output_folder.join(&game_name);
    let _ = std::fs::create_dir_all(&game_folder);

    let file_name = format!("{} {}.DVR.mp4", game_name, stamp);
    let output_path = game_folder.join(&file_name);
    let output_str = output_path.to_string_lossy().to_string();

    eprintln!("[clipsta-daemon] Saving {}s clip → {}", seconds, file_name);

    match session.save_clip(seconds, &output_str) {
        Ok(path) => {
            eprintln!("[clipsta-daemon] ✓ Saved: {}", path);
            chime::play();
        }
        Err(e) => {
            let msg = format!("{}", e);
            if msg.contains("Not enough") || msg.contains("No keyframe") {
                eprintln!("[clipsta-daemon] Buffer not ready yet — wait a few seconds");
            } else {
                eprintln!("[clipsta-daemon] Save failed: {}", msg);
            }
        }
    }
}

fn get_active_window_title() -> Option<String> {
    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return None;
        }
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len == 0 {
            return None;
        }
        let title = String::from_utf16_lossy(&buf[..len as usize]);
        // Clean up title for use as folder name
        let clean = title
            .split(|c: char| c == '|' || c == ':' || c == '*' || c == '?' || c == '"' || c == '<' || c == '>' || c == '/')
            .next()
            .unwrap_or("Desktop")
            .trim()
            .to_string();
        if clean.is_empty() { Some("Desktop".to_string()) } else { Some(clean) }
    }
}

/// Register global hotkeys using raw Windows API.
fn register_global_hotkeys(settings: &AppSettings, tx: std::sync::mpsc::Sender<u32>) {
    let tx30 = tx.clone();
    let tx60 = tx.clone();
    let tx300 = tx.clone();
    let hk30 = settings.hotkey_clip30_sec.clone();
    let hk60 = settings.hotkey_clip1_min.clone();
    let hk300 = settings.hotkey_clip5_min.clone();

    thread::spawn(move || {
        unsafe {
            // Raw Win32 hotkey registration via FFI
            extern "system" {
                fn RegisterHotKey(hwnd: *const std::ffi::c_void, id: i32, modifiers: u32, vk: u32) -> i32;
                fn GetMessageW(msg: *mut RawMsg, hwnd: *const std::ffi::c_void, filter_min: u32, filter_max: u32) -> i32;
            }

            #[repr(C)]
            struct RawMsg {
                hwnd: *const std::ffi::c_void,
                message: u32,
                wparam: usize,
                lparam: isize,
                time: u32,
                pt_x: i32,
                pt_y: i32,
            }

            const WM_HOTKEY: u32 = 0x0312;

            // Register hotkeys: ID 1=30s, ID 2=60s, ID 3=5min
            let (mod30, vk30) = parse_hotkey(&hk30);
            let (mod60, vk60) = parse_hotkey(&hk60);
            let (mod300, vk300) = parse_hotkey(&hk300);

            if vk30 != 0 {
                RegisterHotKey(std::ptr::null(), 1, mod30, vk30);
                eprintln!("[clipsta-daemon] Hotkey 30s: {} (vk=0x{:X})", hk30, vk30);
            }
            if vk60 != 0 {
                RegisterHotKey(std::ptr::null(), 2, mod60, vk60);
                eprintln!("[clipsta-daemon] Hotkey 60s: {} (vk=0x{:X})", hk60, vk60);
            }
            if vk300 != 0 {
                RegisterHotKey(std::ptr::null(), 3, mod300, vk300);
                eprintln!("[clipsta-daemon] Hotkey 5min: {} (vk=0x{:X})", hk300, vk300);
            }

            // Message pump for hotkeys
            let mut msg: RawMsg = std::mem::zeroed();
            loop {
                let ret = GetMessageW(&mut msg, std::ptr::null(), 0, 0);
                if ret <= 0 { break; }
                if msg.message == WM_HOTKEY {
                    let id = msg.wparam;
                    match id {
                        1 => { let _ = tx30.send(30); }
                        2 => { let _ = tx60.send(60); }
                        3 => { let _ = tx300.send(300); }
                        _ => {}
                    }
                }
            }
        }
    });
}

/// Parse a hotkey string like "Ctrl+Shift+G" into (modifiers_bits, virtual_key_code).
fn parse_hotkey(s: &str) -> (u32, u32) {
    let mut modifiers: u32 = 0;
    let mut vk: u32 = 0;

    let parts: Vec<&str> = s.split('+').collect();
    for part in &parts {
        let p = part.trim().to_lowercase();
        match p.as_str() {
            "ctrl" | "control" => modifiers |= 0x0002, // MOD_CONTROL
            "alt" => modifiers |= 0x0001,              // MOD_ALT
            "shift" => modifiers |= 0x0004,            // MOD_SHIFT
            "win" | "super" => modifiers |= 0x0008,    // MOD_WIN
            _ => {
                vk = match p.as_str() {
                    "a" => 0x41, "b" => 0x42, "c" => 0x43, "d" => 0x44,
                    "e" => 0x45, "f" => 0x46, "g" => 0x47, "h" => 0x48,
                    "i" => 0x49, "j" => 0x4A, "k" => 0x4B, "l" => 0x4C,
                    "m" => 0x4D, "n" => 0x4E, "o" => 0x4F, "p" => 0x50,
                    "q" => 0x51, "r" => 0x52, "s" => 0x53, "t" => 0x54,
                    "u" => 0x55, "v" => 0x56, "w" => 0x57, "x" => 0x58,
                    "y" => 0x59, "z" => 0x5A,
                    "f1" => 0x70, "f2" => 0x71, "f3" => 0x72, "f4" => 0x73,
                    "f5" => 0x74, "f6" => 0x75, "f7" => 0x76, "f8" => 0x77,
                    "f9" => 0x78, "f10" => 0x79, "f11" => 0x7A, "f12" => 0x7B,
                    "`" | "~" => 0xC0,
                    _ => 0,
                };
            }
        }
    }

    (modifiers, vk)
}

/// Run hotkey loop (placeholder — actual work done in register_global_hotkeys thread)
fn run_hotkey_loop(running: Arc<AtomicBool>) {
    while running.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
    }
}

/// Named pipe server — allows Tauri UI to send commands (settings changes, manual saves).
fn run_pipe_server(
    session: Arc<CaptureSession>,
    settings: Arc<SettingsStore>,
    running: Arc<AtomicBool>,
    save_tx: std::sync::mpsc::Sender<u32>,
) {
    use clipsta_capture::ipc::{self, CaptureCommand, CaptureResponse, SavedPayload, ErrorPayload, StatusPayload};

    loop {
        if !running.load(Ordering::Relaxed) { break; }

        // Wait for Tauri to connect
        let pipe_file = match ipc::server::create_and_wait_for_client() {
            Ok(f) => f,
            Err(e) => {
                eprintln!("[clipsta-daemon] Pipe server error: {} — retrying in 2s", e);
                thread::sleep(Duration::from_secs(2));
                continue;
            }
        };

        eprintln!("[clipsta-daemon] Tauri UI connected via pipe");

        let pipe_read = match pipe_file.try_clone() {
            Ok(f) => f,
            Err(_) => continue,
        };
        let mut pipe_write = pipe_file;
        let mut reader = std::io::BufReader::new(pipe_read);

        // Command loop for this connection
        loop {
            let cmd: CaptureCommand = match ipc::read_message(&mut reader) {
                Ok(cmd) => cmd,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        eprintln!("[clipsta-daemon] Tauri UI disconnected");
                    } else {
                        eprintln!("[clipsta-daemon] Pipe read error: {}", e);
                    }
                    break;
                }
            };

            let response = match cmd {
                CaptureCommand::Save(payload) => {
                    // Direct save from Tauri UI
                    let _ = save_tx.send(payload.seconds);
                    CaptureResponse::Saved(SavedPayload {
                        path: "pending".to_string(),
                        duration_secs: payload.seconds as f64,
                    })
                }
                CaptureCommand::Status => {
                    CaptureResponse::StatusResp(StatusPayload {
                        is_recording: session.is_recording.load(Ordering::Relaxed),
                        is_saving: session.is_saving.load(Ordering::Relaxed),
                        elapsed_secs: session.elapsed_secs(),
                        frame_drops: session.frame_drops.load(Ordering::Relaxed),
                    })
                }
                CaptureCommand::Quit => {
                    running.store(false, Ordering::SeqCst);
                    let _ = ipc::write_message(&mut pipe_write, &CaptureResponse::Stopped);
                    break;
                }
                _ => {
                    // Start/Stop handled automatically by daemon
                    CaptureResponse::Stopped
                }
            };

            if let Err(_) = ipc::write_message(&mut pipe_write, &response) {
                break;
            }
        }
    }
}
