fn main() {
    tauri_build::build();

    // For release builds, ensure clipsta-capture.exe is available next to the main binary.
    // When building with `cargo tauri build`, both binaries are compiled into target/release/
    // so the CaptureProxy will find clipsta-capture.exe there automatically.
    // For NSIS packaging, we use a custom installer script to include it.
}
