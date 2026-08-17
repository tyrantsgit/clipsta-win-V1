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

// Prevents console window in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io::BufReader;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use parking_lot::Mutex;

use clipsta_tauri_lib::gpu_capture::{
    CaptureOptions, CaptureSession, CompletedSegment,
};
use clipsta_tauri_lib::ipc::{
    self, CaptureCommand, CaptureResponse, ErrorPayload, ReadyPayload,
    SavedPayload, StartPayload,
};
use clipsta_tauri_lib::chime;

fn main() {
    // Initialize COM for Media Foundation (MTA — same as capture threads)
    unsafe {
        use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL};
        let _ = MFStartup(0x0002_0070, MFSTARTUP_FULL);
    }

    eprintln!("[clipsta-capture] Process started (PID {})", std::process::id());

    // Create the capture session
    let session = CaptureSession::new();
    session.warm_start();

    // Create named pipe server and wait for the Tauri process to connect
    let pipe_file = match ipc::server::create_and_wait_for_client() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("[clipsta-capture] FATAL: Failed to create pipe: {}", e);
            std::process::exit(1);
        }
    };

    eprintln!("[clipsta-capture] Client connected");

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
                    eprintln!("[clipsta-capture] Pipe closed — host exited. Shutting down.");
                } else {
                    eprintln!("[clipsta-capture] Read error: {}. Shutting down.", e);
                }
                if session.is_recording.load(Ordering::Relaxed) {
                    session.stop();
                }
                break;
            }
        };

        let response = match cmd {
            CaptureCommand::Start(payload) => handle_start(&session, &payload),
            CaptureCommand::Stop => {
                session.stop();
                CaptureResponse::Stopped
            }
            CaptureCommand::Save(payload) => handle_save(&session, &payload),
            CaptureCommand::Status => CaptureResponse::StatusResp(ipc::StatusPayload {
                is_recording: session.is_recording.load(Ordering::Relaxed),
                is_saving: session.is_saving.load(Ordering::Relaxed),
                elapsed_secs: session.elapsed_secs(),
                frame_drops: session.frame_drops.load(Ordering::Relaxed),
            }),
            CaptureCommand::Quit => {
                if session.is_recording.load(Ordering::Relaxed) {
                    session.stop();
                }
                let _ = ipc::write_message(&mut pipe_write, &CaptureResponse::Stopped);
                break;
            }
        };

        // Send response back
        if let Err(e) = ipc::write_message(&mut pipe_write, &response) {
            eprintln!("[clipsta-capture] Write error: {}. Shutting down.", e);
            break;
        }
    }

    session.cleanup();
    eprintln!("[clipsta-capture] Process exiting");
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
    let on_died = None; // Death detected by pipe read failure on Tauri side

    match session.start(opts, on_segment, on_died) {
        Ok(info) => CaptureResponse::Ready(ReadyPayload {
            width: info.width,
            height: info.height,
            fps: info.fps,
        }),
        Err(e) => CaptureResponse::Error(ErrorPayload {
            message: format!("{}", e),
        }),
    }
}

fn handle_save(session: &CaptureSession, payload: &ipc::SavePayload) -> CaptureResponse {
    match session.save_clip(payload.seconds, &payload.output_path) {
        Ok(path) => {
            // Play chime in the capture process (isolated from WebView2)
            chime::play();
            CaptureResponse::Saved(SavedPayload {
                path,
                duration_secs: payload.seconds as f64,
            })
        }
        Err(e) => CaptureResponse::Error(ErrorPayload {
            message: format!("{}", e),
        }),
    }
}
