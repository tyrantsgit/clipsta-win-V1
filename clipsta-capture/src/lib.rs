//! Clipsta Capture Engine
//!
//! Standalone library for the capture pipeline:
//! - `gpu_capture` — WGC frame capture + D3D11 video processor + H.264 MFT encoder + ring buffer + MP4 muxing
//! - `audio` — WASAPI loopback + mic capture
//! - `ipc` — Named pipe protocol (server + client)
//! - `settings` — Direct settings.json reader (standalone mode)
//! - `tray` — Win32 system tray icon
//! - `hotkeys` — Global hotkey registration

pub mod audio;
pub mod gpu_capture;
pub mod hotkeys;
pub mod ipc;
pub mod settings;
pub mod tray;
