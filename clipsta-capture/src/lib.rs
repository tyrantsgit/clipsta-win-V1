//! Clipsta Capture Engine
//!
//! Standalone library for the capture pipeline:
//! - `gpu_capture` — WGC frame capture + D3D11 video processor + H.264 MFT encoder + ring buffer + MP4 muxing
//! - `audio` — WASAPI loopback + mic capture
//! - `ipc` — Named pipe protocol (server + client)
//! - `chime` — Clip-saved sound effect

pub mod audio;
pub mod chime;
pub mod gpu_capture;
pub mod ipc;
