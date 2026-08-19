//! CaptureProxy — IPC client that wraps communication with clipsta-capture.exe.
//!
//! Features:
//! - Synchronous send/recv over named pipe (serialized by mutex)
//! - Background watchdog thread detects unexpected process death
//! - Auto-respawn with pipe reconnection on crash
//! - Auto-restart recording if it was active when the crash occurred
//! - Tauri event emission to notify the frontend
//! - Crash loop prevention (max 3 respawns in 60 seconds)

use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tauri::{AppHandle, Emitter};

use clipsta_capture::ipc::{
    self, CaptureCommand, CaptureResponse, ReadyPayload, SavePayload, StartPayload, StatusPayload,
};

/// Maximum respawns allowed within the cooldown window.
const MAX_RESPAWNS: usize = 3;
/// Time window for respawn limiting (seconds).
const RESPAWN_WINDOW_SECS: u64 = 60;
/// Watchdog polling interval (ms).
const WATCHDOG_POLL_MS: u64 = 500;

/// Holds the pipe read/write ends protected by a single lock.
struct PipeConn {
    reader: BufReader<std::fs::File>,
    writer: std::fs::File,
}

/// Proxy for the capture process. Managed as Tauri state.
pub struct CaptureProxy {
    /// Tracks whether the remote capture process is recording.
    pub is_recording: Arc<AtomicBool>,
    /// Tracks whether the remote capture process is saving.
    pub is_saving: Arc<AtomicBool>,
    /// Frame drops reported by capture process.
    pub frame_drops: Arc<AtomicU32>,
    /// Width of current capture session.
    pub session_width: Arc<AtomicU32>,
    /// Height of current capture session.
    pub session_height: Arc<AtomicU32>,
    /// FPS of current capture session.
    pub session_fps: Arc<AtomicU32>,
    /// The pipe connection (reader + writer under one lock).
    conn: Arc<Mutex<Option<PipeConn>>>,
    /// Handle to the child capture process.
    child: Arc<Mutex<Option<Child>>>,
    /// Path to the capture executable.
    capture_exe: PathBuf,
    /// Last StartPayload used — stored for auto-restart after crash.
    last_start_payload: Arc<Mutex<Option<StartPayload>>>,
    /// Signal to stop the watchdog thread on shutdown.
    shutdown_flag: Arc<AtomicBool>,
    /// Tracks whether shutdown was intentional (QUIT sent).
    intentional_shutdown: Arc<AtomicBool>,
    /// Timestamps of recent respawns for crash loop detection.
    respawn_times: Arc<Mutex<Vec<Instant>>>,
    /// Whether the crash loop limit has been hit (capture is dead for good).
    pub crash_loop_detected: Arc<AtomicBool>,
}

impl CaptureProxy {
    /// Find the clipsta-capture.exe binary path.
    fn find_capture_exe() -> Result<PathBuf, String> {
        let exe_dir = std::env::current_exe()
            .map_err(|e| format!("current_exe: {}", e))?
            .parent()
            .unwrap()
            .to_path_buf();

        let p1 = exe_dir.join("clipsta-capture.exe");
        if p1.exists() {
            return Ok(p1);
        }
        let p2 = exe_dir.join("resources").join("clipsta-capture.exe");
        if p2.exists() {
            return Ok(p2);
        }
        Err(format!(
            "clipsta-capture.exe not found at {} or {}",
            p1.display(),
            p2.display()
        ))
    }

    /// Spawn the capture process and connect to its pipe.
    fn spawn_process(capture_exe: &PathBuf) -> Result<(Child, PipeConn), String> {
        eprintln!("[clipsta] Spawning capture process: {}", capture_exe.display());

        let child = Command::new(capture_exe)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Failed to spawn clipsta-capture.exe: {}", e))?;

        eprintln!("[clipsta] Capture process spawned (PID {})", child.id());

        // Connect to the pipe (retry for up to 5 seconds while process starts)
        let pipe_file = ipc::client::connect(Duration::from_secs(5))
            .map_err(|e| format!("Failed to connect to capture pipe: {}", e))?;

        eprintln!("[clipsta] Connected to capture pipe");

        let pipe_reader = pipe_file
            .try_clone()
            .map_err(|e| format!("Clone pipe for reader: {}", e))?;

        let conn = PipeConn {
            reader: BufReader::new(pipe_reader),
            writer: pipe_file,
        };

        Ok((child, conn))
    }

    /// Spawn clipsta-capture.exe, connect via named pipe, and start the watchdog.
    pub fn spawn_and_connect(app_handle: AppHandle) -> Result<Self, String> {
        let capture_exe = Self::find_capture_exe()?;
        let (child, conn) = Self::spawn_process(&capture_exe)?;

        let proxy = Self {
            is_recording: Arc::new(AtomicBool::new(false)),
            is_saving: Arc::new(AtomicBool::new(false)),
            frame_drops: Arc::new(AtomicU32::new(0)),
            session_width: Arc::new(AtomicU32::new(1280)),
            session_height: Arc::new(AtomicU32::new(720)),
            session_fps: Arc::new(AtomicU32::new(60)),
            conn: Arc::new(Mutex::new(Some(conn))),
            child: Arc::new(Mutex::new(Some(child))),
            capture_exe,
            last_start_payload: Arc::new(Mutex::new(None)),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            intentional_shutdown: Arc::new(AtomicBool::new(false)),
            respawn_times: Arc::new(Mutex::new(Vec::new())),
            crash_loop_detected: Arc::new(AtomicBool::new(false)),
        };

        // Start the watchdog thread
        proxy.start_watchdog(app_handle);

        Ok(proxy)
    }

    /// Start the background watchdog thread that monitors the child process.
    fn start_watchdog(&self, app_handle: AppHandle) {
        let child = self.child.clone();
        let conn = self.conn.clone();
        let shutdown_flag = self.shutdown_flag.clone();
        let intentional_shutdown = self.intentional_shutdown.clone();
        let is_recording = self.is_recording.clone();
        let last_start_payload = self.last_start_payload.clone();
        let capture_exe = self.capture_exe.clone();
        let respawn_times = self.respawn_times.clone();
        let crash_loop_detected = self.crash_loop_detected.clone();
        let session_width = self.session_width.clone();
        let session_height = self.session_height.clone();
        let session_fps = self.session_fps.clone();
        let frame_drops = self.frame_drops.clone();

        std::thread::Builder::new()
            .name("capture-watchdog".to_string())
            .spawn(move || {
                Self::watchdog_loop(
                    child,
                    conn,
                    shutdown_flag,
                    intentional_shutdown,
                    is_recording,
                    last_start_payload,
                    capture_exe,
                    respawn_times,
                    crash_loop_detected,
                    session_width,
                    session_height,
                    session_fps,
                    frame_drops,
                    app_handle,
                );
            })
            .expect("Failed to spawn watchdog thread");
    }

    /// The watchdog loop — polls child process status, triggers respawn on unexpected death.
    fn watchdog_loop(
        child: Arc<Mutex<Option<Child>>>,
        conn: Arc<Mutex<Option<PipeConn>>>,
        shutdown_flag: Arc<AtomicBool>,
        intentional_shutdown: Arc<AtomicBool>,
        is_recording: Arc<AtomicBool>,
        last_start_payload: Arc<Mutex<Option<StartPayload>>>,
        capture_exe: PathBuf,
        respawn_times: Arc<Mutex<Vec<Instant>>>,
        crash_loop_detected: Arc<AtomicBool>,
        session_width: Arc<AtomicU32>,
        session_height: Arc<AtomicU32>,
        session_fps: Arc<AtomicU32>,
        frame_drops: Arc<AtomicU32>,
        app_handle: AppHandle,
    ) {
        loop {
            std::thread::sleep(Duration::from_millis(WATCHDOG_POLL_MS));

            // Check if we should stop
            if shutdown_flag.load(Ordering::Relaxed) {
                eprintln!("[watchdog] Shutdown signal received, exiting");
                return;
            }

            // Check if child is still alive
            let child_died = {
                let mut child_guard = child.lock();
                if let Some(ref mut c) = *child_guard {
                    match c.try_wait() {
                        Ok(Some(status)) => {
                            eprintln!(
                                "[watchdog] Capture process exited with status: {}",
                                status
                            );
                            // Take ownership so we don't poll a dead process
                            child_guard.take();
                            true
                        }
                        Ok(None) => false, // Still running
                        Err(e) => {
                            eprintln!("[watchdog] Error checking child: {}", e);
                            child_guard.take();
                            true
                        }
                    }
                } else {
                    // No child to watch — might already be respawning
                    false
                }
            };

            if !child_died {
                continue;
            }

            // If shutdown was intentional (we sent QUIT), don't respawn
            if intentional_shutdown.load(Ordering::Relaxed) {
                eprintln!("[watchdog] Intentional shutdown, not respawning");
                return;
            }

            // ── Unexpected death — attempt respawn ──

            let was_recording = is_recording.load(Ordering::SeqCst);
            is_recording.store(false, Ordering::SeqCst);

            // Crash loop detection: check if we've respawned too many times recently
            {
                let mut times = respawn_times.lock();
                let now = Instant::now();
                // Prune old entries outside the window
                times.retain(|t| now.duration_since(*t).as_secs() < RESPAWN_WINDOW_SECS);
                if times.len() >= MAX_RESPAWNS {
                    eprintln!(
                        "[watchdog] Crash loop detected ({} respawns in {}s). Giving up.",
                        MAX_RESPAWNS, RESPAWN_WINDOW_SECS
                    );
                    crash_loop_detected.store(true, Ordering::SeqCst);
                    let _ = app_handle.emit("capture:failed", serde_json::json!({
                        "reason": "Capture process crashed repeatedly. Please restart Clipsta.",
                        "respawns": MAX_RESPAWNS
                    }));
                    return;
                }
                times.push(now);
            }

            // Clear the dead pipe connection
            *conn.lock() = None;

            eprintln!("[watchdog] Attempting respawn...");

            // Notify UI that capture crashed
            let _ = app_handle.emit("capture:crashed", serde_json::json!({
                "was_recording": was_recording
            }));

            // Attempt to respawn
            match Self::spawn_process(&capture_exe) {
                Ok((new_child, new_conn)) => {
                    *child.lock() = Some(new_child);
                    *conn.lock() = Some(new_conn);
                    eprintln!("[watchdog] Respawn successful");

                    // Auto-restart recording if it was active
                    if was_recording {
                        if let Some(payload) = last_start_payload.lock().clone() {
                            eprintln!("[watchdog] Auto-restarting recording...");

                            // Send Start command directly (we have the conn lock context)
                            let restart_result = {
                                let mut conn_guard = conn.lock();
                                if let Some(ref mut c) = *conn_guard {
                                    let send_result = ipc::write_message(
                                        &mut c.writer,
                                        &CaptureCommand::Start(payload),
                                    );
                                    match send_result {
                                        Ok(()) => {
                                            let resp: Result<CaptureResponse, _> =
                                                ipc::read_message(&mut c.reader);
                                            resp.map_err(|e| e.to_string())
                                        }
                                        Err(e) => Err(e.to_string()),
                                    }
                                } else {
                                    Err("No connection after respawn".to_string())
                                }
                            };

                            match restart_result {
                                Ok(CaptureResponse::Ready(info)) => {
                                    is_recording.store(true, Ordering::SeqCst);
                                    session_width.store(info.width, Ordering::SeqCst);
                                    session_height.store(info.height, Ordering::SeqCst);
                                    session_fps.store(info.fps, Ordering::SeqCst);
                                    frame_drops.store(0, Ordering::SeqCst);
                                    eprintln!("[watchdog] Recording auto-restarted successfully");
                                    let _ = app_handle.emit("capture:restarted", serde_json::json!({
                                        "recording_resumed": true,
                                        "width": info.width,
                                        "height": info.height,
                                        "fps": info.fps
                                    }));
                                }
                                Ok(CaptureResponse::Error(e)) => {
                                    eprintln!(
                                        "[watchdog] Failed to restart recording: {}",
                                        e.message
                                    );
                                    let _ = app_handle.emit("capture:restarted", serde_json::json!({
                                        "recording_resumed": false,
                                        "error": e.message
                                    }));
                                }
                                Ok(_) => {
                                    eprintln!("[watchdog] Unexpected response to Start after respawn");
                                    let _ = app_handle.emit("capture:restarted", serde_json::json!({
                                        "recording_resumed": false,
                                        "error": "Unexpected response"
                                    }));
                                }
                                Err(e) => {
                                    eprintln!("[watchdog] Pipe error restarting recording: {}", e);
                                    let _ = app_handle.emit("capture:restarted", serde_json::json!({
                                        "recording_resumed": false,
                                        "error": e
                                    }));
                                }
                            }
                        } else {
                            // Was recording but no saved payload (shouldn't happen)
                            let _ = app_handle.emit("capture:restarted", serde_json::json!({
                                "recording_resumed": false,
                                "error": "No saved recording parameters"
                            }));
                        }
                    } else {
                        // Wasn't recording, just notify the respawn succeeded
                        let _ = app_handle.emit("capture:restarted", serde_json::json!({
                            "recording_resumed": false
                        }));
                    }
                }
                Err(e) => {
                    eprintln!("[watchdog] Respawn FAILED: {}", e);
                    let _ = app_handle.emit("capture:failed", serde_json::json!({
                        "reason": format!("Failed to restart capture process: {}", e)
                    }));
                    // Don't return — loop back and try again on next poll
                    // (respawn_times will eventually trigger crash loop detection)
                }
            }
        }
    }

    /// Send a command and read the response (blocking, serialized by mutex).
    fn send_and_recv(&self, cmd: &CaptureCommand) -> Result<CaptureResponse, String> {
        let mut conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_mut()
            .ok_or_else(|| "Pipe not connected (capture process may be restarting)".to_string())?;

        // Send
        ipc::write_message(&mut conn.writer, cmd)
            .map_err(|e| format!("Pipe write error: {}", e))?;

        // Read response
        ipc::read_message(&mut conn.reader)
            .map_err(|e| format!("Pipe read error: {}", e))
    }

    /// Start capture on the remote process.
    pub fn start(&self, payload: StartPayload) -> Result<ReadyPayload, String> {
        // Store the payload for auto-restart after crash
        *self.last_start_payload.lock() = Some(payload.clone());

        let resp = self.send_and_recv(&CaptureCommand::Start(payload))?;

        match resp {
            CaptureResponse::Ready(info) => {
                self.is_recording.store(true, Ordering::SeqCst);
                self.session_width.store(info.width, Ordering::SeqCst);
                self.session_height.store(info.height, Ordering::SeqCst);
                self.session_fps.store(info.fps, Ordering::SeqCst);
                self.frame_drops.store(0, Ordering::SeqCst);
                Ok(info)
            }
            CaptureResponse::Error(e) => Err(e.message),
            other => Err(format!("Unexpected response to Start: {:?}", other)),
        }
    }

    /// Stop capture on the remote process.
    pub fn stop(&self) -> Result<(), String> {
        let resp = self.send_and_recv(&CaptureCommand::Stop)?;

        match resp {
            CaptureResponse::Stopped => {
                self.is_recording.store(false, Ordering::SeqCst);
                // Clear last payload — if user explicitly stops, don't auto-restart on crash
                *self.last_start_payload.lock() = None;
                Ok(())
            }
            CaptureResponse::Error(e) => Err(e.message),
            other => Err(format!("Unexpected response to Stop: {:?}", other)),
        }
    }

    /// Save a clip via the remote process.
    pub fn save_clip(&self, seconds: u32, output_path: &str) -> Result<String, String> {
        self.is_saving.store(true, Ordering::SeqCst);
        let resp = self.send_and_recv(&CaptureCommand::Save(SavePayload {
            seconds,
            output_path: output_path.to_string(),
        }));
        self.is_saving.store(false, Ordering::SeqCst);

        match resp {
            Ok(CaptureResponse::Saved(info)) => Ok(info.path),
            Ok(CaptureResponse::Error(e)) => Err(e.message),
            Ok(other) => Err(format!("Unexpected response to Save: {:?}", other)),
            Err(e) => Err(e),
        }
    }

    /// Request status from the remote process.
    pub fn status(&self) -> Result<StatusPayload, String> {
        let resp = self.send_and_recv(&CaptureCommand::Status)?;

        match resp {
            CaptureResponse::StatusResp(s) => {
                self.frame_drops.store(s.frame_drops, Ordering::Relaxed);
                Ok(s)
            }
            CaptureResponse::Error(e) => Err(e.message),
            other => Err(format!("Unexpected response to Status: {:?}", other)),
        }
    }

    /// Send QUIT to the capture process and wait for it to exit.
    pub fn shutdown(&self) {
        // Signal the watchdog to stop
        self.shutdown_flag.store(true, Ordering::SeqCst);
        // Mark shutdown as intentional so watchdog doesn't respawn
        self.intentional_shutdown.store(true, Ordering::SeqCst);

        // Send quit command (best-effort)
        if let Some(conn) = self.conn.lock().as_mut() {
            let _ = ipc::write_message(&mut conn.writer, &CaptureCommand::Quit);
        }

        // Wait for child to exit (up to 3 seconds)
        if let Some(mut child) = self.child.lock().take() {
            let start = std::time::Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if start.elapsed() > Duration::from_secs(3) {
                            eprintln!("[clipsta] Capture process didn't exit in 3s, killing");
                            let _ = child.kill();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => break,
                }
            }
        }
    }
}

impl Drop for CaptureProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}
