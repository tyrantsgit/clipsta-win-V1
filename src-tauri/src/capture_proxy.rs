//! CaptureProxy — IPC client that wraps communication with clipsta-capture.exe.
//!
//! Simple synchronous design: send command, read response. No background listener thread.
//! The pipe is guarded by a single Mutex so only one command runs at a time.

use std::io::BufReader;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::ipc::{
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
    /// Handle to the child capture process.
    child: Arc<Mutex<Option<Child>>>,
}

impl CaptureProxy {
    /// Spawn clipsta-capture.exe and connect via named pipe.
    pub fn spawn_and_connect() -> Result<Self, String> {
        // Find clipsta-capture.exe — check multiple locations
        let exe_dir = std::env::current_exe()
            .map_err(|e| format!("current_exe: {}", e))?
            .parent()
            .unwrap()
            .to_path_buf();

        let capture_exe = {
            let p1 = exe_dir.join("clipsta-capture.exe");
            if p1.exists() {
                p1
            } else {
                let p2 = exe_dir.join("resources").join("clipsta-capture.exe");
                if p2.exists() {
                    p2
                } else {
                    return Err(format!(
                        "clipsta-capture.exe not found at {} or {}",
                        p1.display(), p2.display()
                    ));
                }
            }
        };

        eprintln!("[clipsta] Spawning capture process: {}", capture_exe.display());

        let child = Command::new(&capture_exe)
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

        let pipe_reader = pipe_file.try_clone()
            .map_err(|e| format!("Clone pipe for reader: {}", e))?;

        let conn = PipeConn {
            reader: BufReader::new(pipe_reader),
            writer: pipe_file,
        };

        Ok(Self {
            is_recording: Arc::new(AtomicBool::new(false)),
            is_saving: Arc::new(AtomicBool::new(false)),
            frame_drops: Arc::new(AtomicU32::new(0)),
            session_width: Arc::new(AtomicU32::new(1280)),
            session_height: Arc::new(AtomicU32::new(720)),
            session_fps: Arc::new(AtomicU32::new(60)),
            conn: Arc::new(Mutex::new(Some(conn))),
            child: Arc::new(Mutex::new(Some(child))),
        })
    }

    /// Send a command and read the response (blocking, serialized by mutex).
    fn send_and_recv(&self, cmd: &CaptureCommand) -> Result<CaptureResponse, String> {
        let mut conn_guard = self.conn.lock();
        let conn = conn_guard.as_mut()
            .ok_or_else(|| "Pipe not connected".to_string())?;

        // Send
        ipc::write_message(&mut conn.writer, cmd)
            .map_err(|e| format!("Pipe write error: {}", e))?;

        // Read response
        ipc::read_message(&mut conn.reader)
            .map_err(|e| format!("Pipe read error: {}", e))
    }

    /// Start capture on the remote process.
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
                Ok(s)
            }
            CaptureResponse::Error(e) => Err(e.message),
            other => Err(format!("Unexpected response to Status: {:?}", other)),
        }
    }

    /// Send QUIT to the capture process and wait for it to exit.
    pub fn shutdown(&self) {
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
