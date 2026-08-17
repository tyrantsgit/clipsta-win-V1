//! Named pipe IPC protocol for Clipsta split-process architecture.
//!
//! The Tauri process (client) sends commands to clipsta-capture.exe (server)
//! over `\\.\pipe\clipsta-capture`. Messages are newline-delimited JSON.
//!
//! Protocol:
//!   Client → Server: Command messages (START, STOP, SAVE, STATUS, QUIT)
//!   Server → Client: Response messages (READY, STOPPED, SAVED, ERROR, STATUS_RESP)

use serde::{Deserialize, Serialize};

/// Pipe name used for IPC between Tauri and clipsta-capture.
pub const PIPE_NAME: &str = r"\\.\pipe\clipsta-capture";

// ── Client → Server (Commands) ───────────────────────────────────────────────

/// Commands sent from the Tauri process to the capture process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", content = "payload")]
pub enum CaptureCommand {
    /// Start capture with given options.
    Start(StartPayload),
    /// Stop the current capture session.
    Stop,
    /// Save a clip of the last N seconds to the given path.
    Save(SavePayload),
    /// Request current capture status.
    Status,
    /// Shut down the capture process gracefully.
    Quit,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartPayload {
    pub source_id: Option<String>,
    pub fps: u32,
    pub no_audio: bool,
    pub mic_device: Option<String>,
    pub loopback_device: Option<String>,
    pub target_width: Option<u32>,
    pub target_height: Option<u32>,
    pub bitrate_kbps: u32,
    pub buffer_duration: u32,
    pub multi_track_audio: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavePayload {
    pub seconds: u32,
    pub output_path: String,
}

// ── Server → Client (Responses) ──────────────────────────────────────────────

/// Responses sent from the capture process back to the Tauri process.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "resp", content = "payload")]
pub enum CaptureResponse {
    /// Capture started successfully.
    Ready(ReadyPayload),
    /// Capture stopped.
    Stopped,
    /// Clip saved successfully.
    Saved(SavedPayload),
    /// An error occurred.
    Error(ErrorPayload),
    /// Status response.
    StatusResp(StatusPayload),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadyPayload {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedPayload {
    pub path: String,
    pub duration_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusPayload {
    pub is_recording: bool,
    pub is_saving: bool,
    pub elapsed_secs: Option<f64>,
    pub frame_drops: u32,
}

// ── Pipe I/O helpers ─────────────────────────────────────────────────────────

use std::io::{BufRead, Write};

/// Write a JSON message followed by a newline to the given writer.
pub fn write_message<W: Write, T: Serialize>(writer: &mut W, msg: &T) -> std::io::Result<()> {
    let json = serde_json::to_string(msg).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;
    writer.write_all(json.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

/// Read a JSON message from a buffered reader (blocks until newline).
pub fn read_message<R: BufRead, T: for<'de> Deserialize<'de>>(reader: &mut R) -> std::io::Result<T> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "Pipe closed",
        ));
    }
    serde_json::from_str(line.trim()).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("JSON parse error: {} — line: {:?}", e, line.trim()))
    })
}

// ── Named Pipe Server (for capture binary) ───────────────────────────────────

#[cfg(windows)]
pub mod server {
    use super::*;
    use std::fs::File;
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows::Win32::System::Pipes::*;
    use windows::core::PCWSTR;

    /// Create a named pipe server instance and wait for a client to connect.
    /// Returns a File wrapping the pipe handle for use with BufReader/Write.
    pub fn create_and_wait_for_client() -> std::io::Result<File> {
        let pipe_name: Vec<u16> = PIPE_NAME.encode_utf16().chain(std::iter::once(0)).collect();

        let handle: HANDLE = unsafe {
            CreateNamedPipeW(
                PCWSTR(pipe_name.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                1, // max instances
                65536, // out buffer
                65536, // in buffer
                0, // default timeout
                None, // default security
            )
        };

        if handle.is_invalid() {
            return Err(std::io::Error::last_os_error());
        }

        // Wait for a client to connect
        let connected = unsafe { ConnectNamedPipe(handle, None) };
        // ConnectNamedPipe returns Err with ERROR_PIPE_CONNECTED if client connected
        // between Create and Connect. That's still a valid connection.
        if let Err(e) = connected {
            let code = e.code().0 as u32;
            // ERROR_PIPE_CONNECTED = 0x217 = 535
            if code != 535 {
                unsafe { let _ = windows::Win32::Foundation::CloseHandle(handle); }
                return Err(std::io::Error::new(std::io::ErrorKind::Other, format!("ConnectNamedPipe: {}", e)));
            }
        }

        // Wrap in a File for standard I/O traits
        let file = unsafe { File::from_raw_handle(handle.0 as *mut _) };
        Ok(file)
    }
}

// ── Named Pipe Client (for Tauri process) ────────────────────────────────────

#[cfg(windows)]
pub mod client {
    use super::*;
    use std::fs::OpenOptions;
    use std::time::{Duration, Instant};

    /// Connect to the capture process's named pipe.
    /// Retries for up to `timeout` duration if the pipe doesn't exist yet.
    pub fn connect(timeout: Duration) -> std::io::Result<std::fs::File> {
        let start = Instant::now();
        loop {
            match OpenOptions::new()
                .read(true)
                .write(true)
                .open(PIPE_NAME)
            {
                Ok(file) => return Ok(file),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(2) // ERROR_FILE_NOT_FOUND
                    || e.raw_os_error() == Some(231) // ERROR_PIPE_BUSY
                => {
                    if start.elapsed() >= timeout {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("Capture pipe not available after {:?}", timeout),
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(e),
            }
        }
    }
}
