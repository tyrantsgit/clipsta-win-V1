//! clipsta-capture.exe — Standalone capture process for Clipsta 3.0
//!
//! Runs independently as a tray application with its own hotkeys and settings.
//! The Tauri UI process is OPTIONAL — it connects via named pipe IPC when
//! the user opens the editor/library.
//!
//! Flow:
//!   1. Single-instance mutex check
//!   2. Init COM + Media Foundation
//!   3. Load settings from disk
//!   4. Create CaptureSession, warm_start()
//!   5. Start capture immediately (no pipe wait!)
//!   6. IPC pipe server runs in background thread
//!   7. Main thread: tray icon + hotkeys + message pump
//!   8. Save-worker thread: handles clip saves
//!
//! Diagnostic modes (run from command line):
//!   clipsta-capture.exe --probe-encoder    Print GPU encoder capabilities and exit
//!   clipsta-capture.exe --probe-pipeline   Run a short test encode and exit

// Prevents console window in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::BufReader;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::Arc;

use clipsta_capture::gpu_capture::{CaptureOptions, CaptureSession, CompletedSegment};
use clipsta_capture::ipc::{self, CaptureCommand, CaptureResponse, ErrorPayload, StatusPayload};
use clipsta_capture::settings::{self, AppSettings};
use clipsta_capture::hotkeys;
use clipsta_capture::tray::TrayIcon;

mod logging;

/// Convenience macro for formatted logging
macro_rules! log {
    ($($arg:tt)*) => {
        logging::log_args(format_args!($($arg)*))
    };
}

fn main() {
    // ── Parse CLI args ────────────────────────────────────────────────────────
    let args: Vec<String> = std::env::args().collect();
    let probe_encoder = args.iter().any(|a| a == "--probe-encoder");
    let probe_pipeline = args.iter().any(|a| a == "--probe-pipeline");
    let headless = args.iter().any(|a| a == "--headless");

    // For diagnostic modes: attach/allocate a console so output is visible
    if probe_encoder || probe_pipeline {
        unsafe {
            use windows::Win32::System::Console::{AllocConsole, AttachConsole};
            if AttachConsole(u32::MAX).is_err() {
                let _ = AllocConsole();
            }
        }
    }

    // ── Initialize COM + Media Foundation ─────────────────────────────────────
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL};
        let _ = MFStartup(0x0002_0070, MFSTARTUP_FULL);
    }

    // Initialize file logging
    logging::init();

    log!("[clipsta-capture] Process started (PID {})", std::process::id());
    log!("[clipsta-capture] Version 3.0.0 — Standalone mode");

    // ── Diagnostic modes ──────────────────────────────────────────────────────
    if probe_encoder {
        run_probe_encoder();
        log!("[clipsta-capture] --probe-encoder completed");
        return;
    }
    if probe_pipeline {
        run_probe_pipeline();
        log!("[clipsta-capture] --probe-pipeline completed");
        return;
    }

    // ── Single-instance mutex ─────────────────────────────────────────────────
    let _mutex = match create_single_instance_mutex() {
        Some(m) => m,
        None => {
            log!("[clipsta-capture] Another instance is already running. Exiting.");
            return;
        }
    };

    // ── Load settings ─────────────────────────────────────────────────────────
    let settings = settings::load();
    log!("[clipsta-capture] Settings loaded: {}x @ {}fps, buffer={}s, quality={}",
        settings.resolution, settings.fps, settings.buffer_duration, settings.quality);

    // ── Create capture session ────────────────────────────────────────────────
    let session = Arc::new(CaptureSession::new());
    session.warm_start();

    // ── Start capture immediately ─────────────────────────────────────────────
    start_capture(&session, &settings);

    // ── Create channels ───────────────────────────────────────────────────────
    // Save channel: hotkeys/tray/pipe → save worker
    let (save_tx, save_rx) = sync_channel::<u32>(8);
    // Quit channel: tray quit → main
    let (quit_tx, _quit_rx) = sync_channel::<()>(1);

    // ── Save worker thread ────────────────────────────────────────────────────
    let session_for_save = Arc::clone(&session);
    let settings_for_save = settings.clone();
    std::thread::Builder::new()
        .name("save-worker".to_string())
        .spawn(move || {
            log!("[clipsta-capture] Save-worker thread started");
            while let Ok(seconds) = save_rx.recv() {
                do_save(&session_for_save, &settings_for_save, seconds);
            }
            log!("[clipsta-capture] Save-worker thread exiting");
        })
        .expect("Failed to spawn save-worker thread");

    // ── IPC pipe server (background thread) ───────────────────────────────────
    let session_for_pipe = Arc::clone(&session);
    let save_tx_for_pipe = save_tx.clone();
    std::thread::Builder::new()
        .name("ipc-server".to_string())
        .spawn(move || {
            run_pipe_server(session_for_pipe, save_tx_for_pipe);
        })
        .expect("Failed to spawn IPC server thread");

    // ── Tray icon + hotkeys (main thread) — only in standalone mode ──────────
    if headless {
        // Headless mode: no tray, no hotkeys, no message pump.
        // The Tauri app owns the lifecycle — just block until the pipe disconnects
        // or the process is killed.
        log!("[clipsta-capture] Running in headless mode (owned by Tauri app)");
        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            // Check if capture session died unexpectedly
            if !session.is_recording.load(Ordering::Relaxed) {
                log!("[clipsta-capture] Capture stopped in headless mode, exiting");
                break;
            }
        }
    } else {
        let mut tray = TrayIcon::new(save_tx.clone(), quit_tx);

        // Register global hotkeys
        let hk_count = hotkeys::register_all(
            &settings.hotkey_clip30_sec,
            &settings.hotkey_clip1_min,
            &settings.hotkey_clip5_min,
        );
        log!("[clipsta-capture] Registered {} hotkeys", hk_count);

        // Update tooltip to recording state
        if session.is_recording.load(Ordering::Relaxed) {
            tray.set_tooltip("Clipsta \u{2014} Recording");
        } else {
            tray.set_tooltip("Clipsta \u{2014} Ready");
        }

        // ── Main message pump (blocks until quit) ─────────────────────────────────
        log!("[clipsta-capture] Entering message loop");
        tray.run_message_loop();
    }

    // ── Cleanup ───────────────────────────────────────────────────────────────
    log!("[clipsta-capture] Shutting down...");
    hotkeys::unregister_all();

    if session.is_recording.load(Ordering::Relaxed) {
        session.stop();
    }
    session.cleanup();

    log!("[clipsta-capture] Process exiting");
}

// ── Capture start ─────────────────────────────────────────────────────────────

fn start_capture(session: &CaptureSession, settings: &AppSettings) {
    let seg_dir = std::env::temp_dir().join("clipsta_recording");
    if seg_dir.exists() {
        let _ = std::fs::remove_dir_all(&seg_dir);
    }

    let (target_w, target_h) = settings::resolution_to_dimensions(&settings.resolution);
    let bitrate = settings::resolve_bitrate_kbps(
        &settings.resolution,
        settings.fps,
        &settings.quality,
    );

    let mic_device = if settings.capture_mic || settings.audio_source == "mic" || settings.audio_source == "both" {
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

    let no_audio = !settings.capture_audio || settings.audio_source == "none";

    let opts = CaptureOptions {
        source_id: None,
        fps: settings.fps,
        no_audio,
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

    log!("[clipsta-capture] Starting capture: {:?}x{:?} @ {}fps, {}kbps, buffer={}s",
        target_w, target_h, settings.fps, bitrate, settings.buffer_duration);

    let on_segment = Box::new(|_seg: CompletedSegment| {});
    let on_died = None;

    match session.start(opts, on_segment, on_died) {
        Ok(info) => {
            log!("[clipsta-capture] Capture started: {}x{} @ {}fps", info.width, info.height, info.fps);
        }
        Err(e) => {
            log!("[clipsta-capture] Capture start FAILED: {}", e);
        }
    }
}

// ── Save logic ────────────────────────────────────────────────────────────────

fn do_save(session: &CaptureSession, settings: &AppSettings, seconds: u32) {
    if !session.is_recording.load(Ordering::Relaxed) {
        log!("[clipsta-capture] Cannot save — not recording");
        return;
    }
    if session.is_saving.load(Ordering::Relaxed) {
        log!("[clipsta-capture] Save already in progress, skipping");
        return;
    }

    // Generate ShadowPlay-style filename
    let now = chrono::Local::now();
    let cs = now.format("%f").to_string();
    let centiseconds = &cs[..2.min(cs.len())];
    let stamp = format!("{}.{}", now.format("%Y.%m.%d - %H.%M.%S"), centiseconds);

    let game_name = get_game_name(settings.game_detect);

    let output_folder = PathBuf::from(&settings.output_folder);
    let _ = std::fs::create_dir_all(&output_folder);

    // Only create game subfolder for actual games (not Desktop, not browser titles)
    let is_generic = game_name.is_empty()
        || game_name == "Desktop"
        || game_name == "Clipsta"
        || game_name.contains(" - ");
    let game_folder = if is_generic {
        output_folder.clone()
    } else {
        let gf = output_folder.join(&game_name);
        let _ = std::fs::create_dir_all(&gf);
        gf
    };

    let file_name = format!("{} {}.DVR.mp4", game_name, stamp);
    let output_path = game_folder.join(&file_name);
    let output_str = output_path.to_string_lossy().to_string();

    log!("[clipsta-capture] Saving {}s clip → {}", seconds, file_name);

    match session.save_clip(seconds, &output_str) {
        Ok(path) => {
            log!("[clipsta-capture] ✓ Saved: {}", path);
        }
        Err(e) => {
            let msg = format!("{}", e);
            if msg.contains("Not enough") || msg.contains("No keyframe") {
                log!("[clipsta-capture] Buffer not ready — wait a few seconds");
            } else {
                log!("[clipsta-capture] Save failed: {}", msg);
            }
        }
    }
}

/// Get the active window title for use as game/folder name.
fn get_game_name(game_detect: bool) -> String {
    if !game_detect {
        return "Desktop".to_string();
    }

    unsafe {
        use windows::Win32::UI::WindowsAndMessaging::*;
        let hwnd = GetForegroundWindow();
        if hwnd.0.is_null() {
            return "Desktop".to_string();
        }
        let mut buf = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut buf);
        if len == 0 {
            return "Desktop".to_string();
        }
        let title = String::from_utf16_lossy(&buf[..len as usize]);

        // Strip zero-width and invisible Unicode characters
        let title: String = title
            .chars()
            .filter(|c| {
                !matches!(
                    *c,
                    '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{00AD}'
                    | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}'
                    | '\u{2060}'..='\u{2064}'
                )
            })
            .collect();

        // Clean up browser/app suffixes
        let cleaned = title
            .trim()
            .trim_end_matches(" - Google Chrome")
            .trim_end_matches(" - Mozilla Firefox")
            .trim_end_matches(" - Microsoft Edge")
            .trim_end_matches(" - Visual Studio Code")
            .trim_end_matches(" - Discord")
            .to_string();

        // Sanitize for filesystem
        let sanitized: String = cleaned
            .chars()
            .map(|c| match c {
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
                _ => c,
            })
            .collect();

        if sanitized.is_empty() {
            "Desktop".to_string()
        } else {
            sanitized
        }
    }
}

// ── IPC Pipe Server (background thread) ───────────────────────────────────────

fn run_pipe_server(session: Arc<CaptureSession>, save_tx: SyncSender<u32>) {
    log!("[clipsta-capture] IPC pipe server thread started");

    loop {
        // Create pipe and wait for client (blocks this thread only)
        let pipe_file = match ipc::server::create_and_wait_for_client() {
            Ok(f) => f,
            Err(e) => {
                log!("[clipsta-capture] IPC: Failed to create pipe: {}. Retrying in 1s...", e);
                std::thread::sleep(std::time::Duration::from_secs(1));
                continue;
            }
        };

        log!("[clipsta-capture] IPC: Client connected");

        let pipe_read = match pipe_file.try_clone() {
            Ok(f) => f,
            Err(e) => {
                log!("[clipsta-capture] IPC: Failed to clone pipe handle: {}", e);
                continue;
            }
        };
        let mut pipe_write = pipe_file;
        let mut reader = BufReader::new(pipe_read);

        // Command loop for this connection
        loop {
            let cmd: CaptureCommand = match ipc::read_message(&mut reader) {
                Ok(cmd) => cmd,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        log!("[clipsta-capture] IPC: Client disconnected");
                    } else {
                        log!("[clipsta-capture] IPC: Read error: {}", e);
                    }
                    break; // Go back to waiting for next client
                }
            };

            let response = match cmd {
                CaptureCommand::Start(_payload) => {
                    // Report current state — capture is already running with correct settings
                    // from start_capture() at launch (which uses audio_source for mic).
                    log!("[clipsta-capture] IPC: Start command (capture already active)");
                    if session.is_recording.load(Ordering::Relaxed) {
                        CaptureResponse::Ready(ipc::ReadyPayload {
                            width: 0,
                            height: 0,
                            fps: 0,
                        })
                    } else {
                        // Not yet recording — start with the payload from Tauri
                        log!("[clipsta-capture] IPC: Starting capture (mic: {:?})", _payload.mic_device);
                        let seg_dir = std::env::temp_dir().join(format!("clipsta_segments_{}", std::process::id()));
                        let _ = std::fs::create_dir_all(&seg_dir);
                        let capture_opts = CaptureOptions {
                            source_id: _payload.source_id,
                            fps: _payload.fps,
                            no_audio: _payload.no_audio,
                            mic_device: _payload.mic_device,
                            loopback_device: _payload.loopback_device,
                            target_width: _payload.target_width,
                            target_height: _payload.target_height,
                            bitrate_kbps: _payload.bitrate_kbps,
                            segment_duration: 3,
                            buffer_duration: _payload.buffer_duration,
                            segment_dir: seg_dir,
                            multi_track_audio: _payload.multi_track_audio,
                            warm_cache: None,
                        };
                        let on_segment = Box::new(|_seg: CompletedSegment| {});
                        match session.start(capture_opts, on_segment, None) {
                            Ok(info) => CaptureResponse::Ready(ipc::ReadyPayload {
                                width: info.width,
                                height: info.height,
                                fps: info.fps,
                            }),
                            Err(e) => CaptureResponse::Error(ErrorPayload {
                                message: format!("Start failed: {}", e),
                            }),
                        }
                    }
                }
                CaptureCommand::Stop => {
                    // In standalone mode, don't stop capture from pipe
                    log!("[clipsta-capture] IPC: Stop command (ignored in standalone mode)");
                    CaptureResponse::Stopped
                }
                CaptureCommand::Save(ref payload) => {
                    log!("[clipsta-capture] IPC: Save command ({}s)", payload.seconds);
                    // Forward to save worker via channel
                    let _ = save_tx.try_send(payload.seconds);
                    // Return an immediate ack — actual save happens async
                    CaptureResponse::Saved(ipc::SavedPayload {
                        path: payload.output_path.clone(),
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
                    // In standalone mode, ignore quit from pipe — capture owns its lifecycle
                    log!("[clipsta-capture] IPC: Quit command (ignored in standalone mode)");
                    CaptureResponse::Stopped
                }
            };

            if let Err(e) = ipc::write_message(&mut pipe_write, &response) {
                log!("[clipsta-capture] IPC: Write error: {}. Disconnecting client.", e);
                break;
            }
        }

        // Client disconnected — loop back to create new pipe instance
        log!("[clipsta-capture] IPC: Waiting for next client...");
    }
}

// ── Single-instance mutex ─────────────────────────────────────────────────────

/// Creates a named mutex to ensure only one instance of clipsta-capture runs.
/// Returns None if another instance already owns the mutex.
fn create_single_instance_mutex() -> Option<MutexHandle> {
    use windows::core::w;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::CreateMutexW;

    unsafe {
        let handle = CreateMutexW(None, true, w!("Global\\ClipstaCaptureV3Mutex")).ok()?;

        // Check if we actually own it (vs it already existed)
        let last_error = windows::Win32::Foundation::GetLastError();
        if last_error.0 == 183 {
            // ERROR_ALREADY_EXISTS
            let _ = CloseHandle(handle);
            return None;
        }

        Some(MutexHandle(handle))
    }
}

/// RAII wrapper for the named mutex handle.
struct MutexHandle(windows::Win32::Foundation::HANDLE);

impl Drop for MutexHandle {
    fn drop(&mut self) {
        unsafe {
            use windows::Win32::System::Threading::ReleaseMutex;
            let _ = ReleaseMutex(self.0);
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

// ── Diagnostic: --probe-encoder ───────────────────────────────────────────────

fn run_probe_encoder() {
    println!("=== Clipsta Capture — Encoder Probe ===");
    println!();

    unsafe {
        use windows::Win32::Media::MediaFoundation::*;
        use std::ptr;

        let in_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let out_info = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };

        // Enumerate hardware encoders
        let flags = MFT_ENUM_FLAG(MFT_ENUM_FLAG_HARDWARE.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        let mut activates_ptr: *mut Option<IMFActivate> = ptr::null_mut();
        let mut count: u32 = 0;

        let hr = MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags,
            Some(&in_info),
            Some(&out_info),
            &mut activates_ptr,
            &mut count,
        );

        match hr {
            Ok(()) => {
                println!("Hardware H.264 encoders found: {}", count);
                if count > 0 && !activates_ptr.is_null() {
                    let activates = std::slice::from_raw_parts(activates_ptr, count as usize);
                    for (i, slot) in activates.iter().enumerate() {
                        if let Some(activate) = slot {
                            let name = get_mft_name(activate);
                            println!("  [{}] {}", i, name);
                        }
                    }
                }
                if !activates_ptr.is_null() {
                    windows::Win32::System::Com::CoTaskMemFree(Some(activates_ptr as *const _));
                }
            }
            Err(e) => println!("MFTEnumEx failed: {}", e),
        }

        // Also enumerate software encoders
        let flags_sw = MFT_ENUM_FLAG(MFT_ENUM_FLAG_SYNCMFT.0 | MFT_ENUM_FLAG_ASYNCMFT.0 | MFT_ENUM_FLAG_SORTANDFILTER.0);
        let mut sw_ptr: *mut Option<IMFActivate> = ptr::null_mut();
        let mut sw_count: u32 = 0;

        let hr = MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            flags_sw,
            Some(&in_info),
            Some(&out_info),
            &mut sw_ptr,
            &mut sw_count,
        );

        if let Ok(()) = hr {
            println!("\nSoftware H.264 encoders found: {}", sw_count);
            if sw_count > 0 && !sw_ptr.is_null() {
                let activates = std::slice::from_raw_parts(sw_ptr, sw_count as usize);
                for (i, slot) in activates.iter().enumerate() {
                    if let Some(activate) = slot {
                        let name = get_mft_name(activate);
                        println!("  [{}] {}", i, name);
                    }
                }
            }
            if !sw_ptr.is_null() {
                windows::Win32::System::Com::CoTaskMemFree(Some(sw_ptr as *const _));
            }
        }
    }

    // Print GPU info via DXGI
    println!();
    unsafe {
        use windows::Win32::Graphics::Dxgi::*;

        if let Ok(factory) = CreateDXGIFactory1::<IDXGIFactory1>() {
            let mut i = 0u32;
            println!("DXGI Adapters:");
            loop {
                match factory.EnumAdapters1(i) {
                    Ok(adapter) => {
                        if let Ok(desc) = adapter.GetDesc1() {
                            let name = String::from_utf16_lossy(
                                &desc.Description[..desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len())]
                            );
                            println!("  [{}] {} (VRAM: {} MB, Shared: {} MB)",
                                i, name,
                                desc.DedicatedVideoMemory / (1024 * 1024),
                                desc.SharedSystemMemory / (1024 * 1024),
                            );
                        }
                        i += 1;
                    }
                    Err(_) => break,
                }
            }
        }
    }

    println!("\nProbe complete.");
}

// ── Diagnostic: --probe-pipeline ──────────────────────────────────────────────

fn run_probe_pipeline() {
    println!("=== Clipsta Capture — Pipeline Probe ===");
    println!();
    println!("Testing: D3D11 device → Video Processor → H.264 Encoder");
    println!();

    let session = CaptureSession::new();

    let seg_dir = std::env::temp_dir().join("clipsta_probe");
    if seg_dir.exists() {
        let _ = std::fs::remove_dir_all(&seg_dir);
    }

    let opts = CaptureOptions {
        source_id: None,
        fps: 60,
        no_audio: true,
        mic_device: None,
        loopback_device: None,
        target_width: Some(1920),
        target_height: Some(1088),
        bitrate_kbps: 8000,
        segment_duration: 3,
        buffer_duration: 5,
        segment_dir: seg_dir.clone(),
        multi_track_audio: false,
        warm_cache: Some(session.warm_cache.clone()),
    };

    let on_segment = Box::new(|_seg: CompletedSegment| {});
    let on_died = None;

    println!("Starting capture pipeline (1920x1088 @ 60fps, 8 Mbps)...");
    match session.start(opts, on_segment, on_died) {
        Ok(info) => {
            println!("✓ Pipeline started: {}x{} @ {}fps", info.width, info.height, info.fps);
            println!("  Running for 3 seconds...");
            std::thread::sleep(std::time::Duration::from_secs(3));

            let drops = session.frame_drops.load(Ordering::Relaxed);
            println!("  Frame drops: {}", drops);
            if drops == 0 {
                println!("✓ No frame drops — pipeline is healthy");
            } else if drops < 10 {
                println!("⚠ Minor frame drops ({}) — GPU may be under load", drops);
            } else {
                println!("✗ Significant frame drops ({}) — encoder may be struggling", drops);
            }

            session.stop();
            println!("✓ Pipeline stopped cleanly");
        }
        Err(e) => {
            println!("✗ Pipeline FAILED to start: {}", e);
            println!();
            println!("This usually means:");
            println!("  - No hardware H.264 encoder available");
            println!("  - GPU driver issue (try updating)");
            println!("  - Windows Graphics Capture not available");
        }
    }

    session.cleanup();
    let _ = std::fs::remove_dir_all(&seg_dir);
    println!("\nProbe complete.");
}

/// Get the friendly name of an MFT activate object.
unsafe fn get_mft_name(activate: &windows::Win32::Media::MediaFoundation::IMFActivate) -> String {
    use windows::Win32::Media::MediaFoundation::MFT_FRIENDLY_NAME_Attribute;
    let mut name_ptr = windows::core::PWSTR::null();
    let mut name_len: u32 = 0;
    match activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut name_ptr, &mut name_len) {
        Ok(()) if !name_ptr.is_null() => {
            let s = name_ptr.to_string().unwrap_or_else(|_| "(invalid utf16)".to_string());
            windows::Win32::System::Com::CoTaskMemFree(Some(name_ptr.as_ptr() as *const _));
            s
        }
        _ => "(unknown)".to_string(),
    }
}
