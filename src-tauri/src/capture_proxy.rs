//! CaptureProxy — IPC client that connects to the already-running clipsta-capture.exe.
//!
//! In Clipsta 3.0, the capture process runs independently as a tray app.
//! Tauri connects to it (rather than spawning it) when the user opens
//! the editor/library/settings UI.
//!
//! Features:
//! - Connects to existing named pipe (clipsta-capture.exe is already running)
//! - Fallback: spawns clipsta-capture.exe if not already running
//! - Synchronous send/recv over named pipe (serialized by mutex)
//! - Reconnection on pipe errors

use std::io::BufReader;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tauri::AppHandle;

use clipsta_capture::ipc::{
    self, CaptureCommand, CaptureResponse, ReadyPayload, SavePayload, StartPayload, StatusPayload,
};

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
    /// Handle to a child capture process (only if we spawned it as fallback).
    child: Arc<Mutex<Option<Child>>>,
    /// Path to the capture executable (for fallback spawn).
    capture_exe: PathBuf,
    /// Whether we spawned the process (vs connecting to existing).
    pub we_own_process: Arc<AtomicBool>,
    /// Crash loop detection flag.
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

        // Check next to our exe (installed layout)
        let p1 = exe_dir.join("clipsta-capture.exe");
        if p1.exists() {
            return Ok(p1);
        }
        // Check resources subfolder
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

    /// Try to connect to an already-running capture process.
    /// If that fails, spawn one and connect.
    pub fn connect_or_spawn(app_handle: AppHandle) -> Result<Self, String> {
        let capture_exe = Self::find_capture_exe()?;

        // First, try connecting to existing pipe (capture already running from startup)
        let (conn, child, we_own) = match ipc::client::connect(Duration::from_secs(2)) {
            Ok(pipe_file) => {
                eprintln!("[clipsta] Connected to existing capture process");
                let pipe_reader = pipe_file
                    .try_clone()
                    .map_err(|e| format!("Clone pipe for reader: {}", e))?;
                let conn = PipeConn {
                    reader: BufReader::new(pipe_reader),
                    writer: pipe_file,
                };
                (conn, None, false)
            }
            Err(_) => {
                // Capture process not running — spawn it as fallback
                eprintln!("[clipsta] Capture process not running, spawning as fallback...");
                let (child, conn) = Self::spawn_process(&capture_exe)?;
                (conn, Some(child), true)
            }
        };

        let proxy = Self {
            is_recording: Arc::new(AtomicBool::new(false)),
            is_saving: Arc::new(AtomicBool::new(false)),
            frame_drops: Arc::new(AtomicU32::new(0)),
            session_width: Arc::new(AtomicU32::new(1920)),
            session_height: Arc::new(AtomicU32::new(1080)),
            session_fps: Arc::new(AtomicU32::new(60)),
            conn: Arc::new(Mutex::new(Some(conn))),
            child: Arc::new(Mutex::new(child)),
            capture_exe,
            we_own_process: Arc::new(AtomicBool::new(we_own)),
            crash_loop_detected: Arc::new(AtomicBool::new(false)),
        };

        // Query initial status from the capture process
        let _ = app_handle; // Reserved for future event emission
        if let Ok(status) = proxy.status() {
            proxy.is_recording.store(status.is_recording, Ordering::SeqCst);
        }

        Ok(proxy)
    }

    /// Spawn the capture process (fallback when it's not already running).
    fn spawn_process(capture_exe: &PathBuf) -> Result<(Child, PipeConn), String> {
        eprintln!("[clipsta] Spawning capture process: {}", capture_exe.display());

        let child = Command::new(capture_exe)
            .arg("--headless")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| format!("Failed to spawn clipsta-capture.exe: {}", e))?;

        eprintln!("[clipsta] Capture process spawned (PID {})", child.id());

        // Wait for the pipe to become available (capture needs a moment to start)
        let pipe_file = ipc::client::connect(Duration::from_secs(10))
            .map_err(|e| format!("Failed to connect to capture pipe after spawn: {}", e))?;

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

    /// Send a command and read the response (blocking, serialized by mutex).
    fn send_and_recv(&self, cmd: &CaptureCommand) -> Result<CaptureResponse, String> {
        let mut conn_guard = self.conn.lock();
        let conn = conn_guard
            .as_mut()
            .ok_or_else(|| "Pipe not connected (capture process may not be running)".to_string())?;

        // Send
        ipc::write_message(&mut conn.writer, cmd)
            .map_err(|e| format!("Pipe write error: {}", e))?;

        // Read response
        ipc::read_message(&mut conn.reader)
            .map_err(|e| format!("Pipe read error: {}", e))
    }

    /// Attempt to reconnect to the capture process pipe.
    pub fn reconnect(&self) -> Result<(), String> {
        let pipe_file = ipc::client::connect(Duration::from_secs(3))
            .map_err(|e| format!("Reconnect failed: {}", e))?;

        let pipe_reader = pipe_file
            .try_clone()
            .map_err(|e| format!("Clone pipe for reader on reconnect: {}", e))?;

        let conn = PipeConn {
            reader: BufReader::new(pipe_reader),
            writer: pipe_file,
        };

        *self.conn.lock() = Some(conn);
        eprintln!("[clipsta] Reconnected to capture pipe");
        Ok(())
    }

    /// Start capture on the remote process.
    /// In 3.0, the capture process is likely already recording — this is mostly
    /// used if Tauri spawned it as a fallback and needs to start recording.
    pub fn start(&self, payload: StartPayload) -> Result<ReadyPayload, String> {
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
                if s.is_recording {
                    self.is_recording.store(true, Ordering::SeqCst);
                }
                Ok(s)
            }
            CaptureResponse::Error(e) => Err(e.message),
            other => Err(format!("Unexpected response to Status: {:?}", other)),
        }
    }

    /// Disconnect from the capture process. Does NOT kill it (it's independent).
    pub fn shutdown(&self) {
        // Drop the pipe connection
        *self.conn.lock() = None;

        // Only kill the child if WE spawned it as a fallback
        if self.we_own_process.load(Ordering::Relaxed) {
            if let Some(mut child) = self.child.lock().take() {
                // Send quit command first (best-effort, pipe may already be dropped)
                eprintln!("[clipsta] Stopping fallback capture process");
                let _ = child.kill();
            }
        }
    }
}

impl Drop for CaptureProxy {
    fn drop(&mut self) {
        self.shutdown();
    }
}

// Keep spawn_and_connect as an alias for backward compatibility with the rest of lib.rs
impl CaptureProxy {
    pub fn spawn_and_connect(app_handle: AppHandle) -> Result<Self, String> {
        Self::connect_or_spawn(app_handle)
    }
}
