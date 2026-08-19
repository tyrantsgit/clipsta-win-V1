//! Clip save chime — plays a short sound effect when a clip is saved.
//!
//! Completely isolated from the capture/audio pipeline:
//! - Runs on its own thread (fire-and-forget)
//! - Uses Windows PlaySound API (system audio, not WASAPI)
//! - Never touches any mutex, ring buffer, or audio capture state
//! - Gracefully silent if the WAV file is missing

/// Play the clip-saved chime. Non-blocking, fire-and-forget.
pub fn play() {
    std::thread::spawn(|| {
        let wav_path = find_wav();
        let Some(path) = wav_path else { return; };

        let wide: Vec<u16> = path
            .to_string_lossy()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        unsafe {
            let _ = windows::Win32::Media::Audio::PlaySoundW(
                windows::core::PCWSTR(wide.as_ptr()),
                None,
                windows::Win32::Media::Audio::SND_FILENAME
                    | windows::Win32::Media::Audio::SND_ASYNC
                    | windows::Win32::Media::Audio::SND_NODEFAULT,
            );
        }
    });
}

/// Find clip-saved.wav in possible locations
fn find_wav() -> Option<std::path::PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();

    // Installed location: next to exe in resources/
    let p1 = exe_dir.join("resources").join("clip-saved.wav");
    if p1.exists() { return Some(p1); }

    // Tauri dev mode: src-tauri/resources/
    let p2 = exe_dir.join("..").join("..").join("resources").join("clip-saved.wav");
    if p2.exists() { return Some(p2); }

    // Flat next to exe
    let p3 = exe_dir.join("clip-saved.wav");
    if p3.exists() { return Some(p3); }

    None
}
