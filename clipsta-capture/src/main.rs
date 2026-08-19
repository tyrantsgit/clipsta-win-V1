//! clipsta-capture.exe — Standalone capture process for Clipsta v2.3+
//!
//! Runs the GPU capture pipeline (WGC + WASAPI + H.264 encoder) in isolation
//! from WebView2 to eliminate USB audio scheduling interference.
//!
//! Like Clipsta Lite: handles capture, saves, and chime entirely in-process.
//! The Tauri UI process only sends START/STOP/QUIT commands.
//! Hotkey saves are triggered by Tauri sending SAVE commands over the pipe.
//!
//! Communication: \\.\pipe\clipsta-capture (JSON newline-delimited)
//!
//! Diagnostic modes (run from command line):
//!   clipsta-capture.exe --probe-encoder    Print GPU encoder capabilities and exit
//!   clipsta-capture.exe --probe-pipeline   Run a short test encode and exit

// Prevents console window in release builds (but NOT when run with --probe-* flags)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::BufReader;
use std::sync::atomic::Ordering;

use clipsta_capture::gpu_capture::{
    CaptureOptions, CaptureSession, CompletedSegment,
};
use clipsta_capture::ipc::{
    self, CaptureCommand, CaptureResponse, ErrorPayload, ReadyPayload,
    SavedPayload, StartPayload,
};
use clipsta_capture::chime;

mod logging;

/// Convenience macro for formatted logging
macro_rules! log {
    ($($arg:tt)*) => {
        logging::log_args(format_args!($($arg)*))
    };
}

fn main() {
    // Parse CLI args before anything else
    let args: Vec<String> = std::env::args().collect();
    let probe_encoder = args.iter().any(|a| a == "--probe-encoder");
    let probe_pipeline = args.iter().any(|a| a == "--probe-pipeline");

    // For diagnostic modes: attach/allocate a console so output is visible in release builds
    if probe_encoder || probe_pipeline {
        unsafe {
            use windows::Win32::System::Console::{AllocConsole, AttachConsole};
            // Try attaching to parent console first (running from cmd/powershell)
            if AttachConsole(u32::MAX).is_err() {
                // No parent console — allocate our own
                let _ = AllocConsole();
            }
        }
    }

    // Initialize COM for Media Foundation (MTA — same as capture threads)
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL};
        let _ = MFStartup(0x0002_0070, MFSTARTUP_FULL);
    }

    // Initialize file logging (non-blocking background writer)
    logging::init();

    log!("[clipsta-capture] Process started (PID {})", std::process::id());
    log!("[clipsta-capture] Version 2.3.2");

    // ── Diagnostic modes ──────────────────────────────────────────────────────
    if probe_encoder {
        run_probe_encoder();
        // Also write probe output to log file for easy retrieval
        log!("[clipsta-capture] --probe-encoder completed");
        return;
    }
    if probe_pipeline {
        run_probe_pipeline();
        log!("[clipsta-capture] --probe-pipeline completed");
        return;
    }

    // ── Normal capture server mode ────────────────────────────────────────────

    // Create the capture session
    let session = CaptureSession::new();
    session.warm_start();

    // Create named pipe server and wait for the Tauri process to connect
    let pipe_file = match ipc::server::create_and_wait_for_client() {
        Ok(f) => f,
        Err(e) => {
            log!("[clipsta-capture] FATAL: Failed to create pipe: {}", e);
            std::process::exit(1);
        }
    };

    log!("[clipsta-capture] Client connected");

    // Use one handle for reading, one for writing
    let pipe_read = pipe_file.try_clone().expect("Failed to clone pipe handle");
    let mut pipe_write = pipe_file;

    let mut reader = BufReader::new(pipe_read);

    // Main command loop — simple request/response like Clipsta Lite
    loop {
        let cmd: CaptureCommand = match ipc::read_message(&mut reader) {
            Ok(cmd) => cmd,
            Err(e) => {
                if e.kind() == std::io::ErrorKind::UnexpectedEof {
                    log!("[clipsta-capture] Pipe closed — host exited. Shutting down.");
                } else {
                    log!("[clipsta-capture] Read error: {}. Shutting down.", e);
                }
                if session.is_recording.load(Ordering::Relaxed) {
                    session.stop();
                }
                break;
            }
        };

        let response = match cmd {
            CaptureCommand::Start(payload) => {
                log!("[clipsta-capture] CMD: Start ({}x{} @ {}fps, buffer={}s, bitrate={}kbps)",
                    payload.target_width.unwrap_or(0),
                    payload.target_height.unwrap_or(0),
                    payload.fps,
                    payload.buffer_duration,
                    payload.bitrate_kbps,
                );
                handle_start(&session, &payload)
            }
            CaptureCommand::Stop => {
                log!("[clipsta-capture] CMD: Stop");
                session.stop();
                CaptureResponse::Stopped
            }
            CaptureCommand::Save(ref payload) => {
                log!("[clipsta-capture] CMD: Save ({}s → {})", payload.seconds, payload.output_path);
                handle_save(&session, payload)
            }
            CaptureCommand::Status => CaptureResponse::StatusResp(ipc::StatusPayload {
                is_recording: session.is_recording.load(Ordering::Relaxed),
                is_saving: session.is_saving.load(Ordering::Relaxed),
                elapsed_secs: session.elapsed_secs(),
                frame_drops: session.frame_drops.load(Ordering::Relaxed),
            }),
            CaptureCommand::Quit => {
                log!("[clipsta-capture] CMD: Quit");
                if session.is_recording.load(Ordering::Relaxed) {
                    session.stop();
                }
                let _ = ipc::write_message(&mut pipe_write, &CaptureResponse::Stopped);
                break;
            }
        };

        // Send response back
        if let Err(e) = ipc::write_message(&mut pipe_write, &response) {
            log!("[clipsta-capture] Write error: {}. Shutting down.", e);
            break;
        }
    }

    session.cleanup();
    log!("[clipsta-capture] Process exiting");
}

fn handle_start(session: &CaptureSession, payload: &StartPayload) -> CaptureResponse {
    let seg_dir = std::env::temp_dir().join("clipsta_recording");
    if seg_dir.exists() {
        let _ = std::fs::remove_dir_all(&seg_dir);
    }

    let opts = CaptureOptions {
        source_id: payload.source_id.clone(),
        fps: payload.fps,
        no_audio: payload.no_audio,
        mic_device: payload.mic_device.clone(),
        loopback_device: payload.loopback_device.clone(),
        target_width: payload.target_width,
        target_height: payload.target_height,
        bitrate_kbps: payload.bitrate_kbps,
        segment_duration: 3,
        buffer_duration: payload.buffer_duration,
        segment_dir: seg_dir,
        multi_track_audio: payload.multi_track_audio,
        warm_cache: Some(session.warm_cache.clone()),
    };

    let on_segment = Box::new(|_seg: CompletedSegment| {});
    let on_died = None;

    match session.start(opts, on_segment, on_died) {
        Ok(info) => {
            log!("[clipsta-capture] Recording started: {}x{} @ {}fps", info.width, info.height, info.fps);
            CaptureResponse::Ready(ReadyPayload {
                width: info.width,
                height: info.height,
                fps: info.fps,
            })
        }
        Err(e) => {
            log!("[clipsta-capture] Start FAILED: {}", e);
            CaptureResponse::Error(ErrorPayload {
                message: format!("{}", e),
            })
        }
    }
}

fn handle_save(session: &CaptureSession, payload: &ipc::SavePayload) -> CaptureResponse {
    match session.save_clip(payload.seconds, &payload.output_path) {
        Ok(path) => {
            log!("[clipsta-capture] Clip saved: {}", path);
            chime::play();
            CaptureResponse::Saved(SavedPayload {
                path,
                duration_secs: payload.seconds as f64,
            })
        }
        Err(e) => {
            log!("[clipsta-capture] Save FAILED: {}", e);
            CaptureResponse::Error(ErrorPayload {
                message: format!("{}", e),
            })
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

    // Create a test capture with a tiny buffer, run for 2 seconds, then stop
    let seg_dir = std::env::temp_dir().join("clipsta_probe");
    if seg_dir.exists() {
        let _ = std::fs::remove_dir_all(&seg_dir);
    }

    let opts = CaptureOptions {
        source_id: None,
        fps: 60,
        no_audio: true, // Skip audio for probe
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
