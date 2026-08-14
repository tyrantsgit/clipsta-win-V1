# Clipsta v2.3.1

**The gaming clip recorder that just works.** Always recording, always ready. Save the last 30 seconds, 1 minute, or 5 minutes of gameplay with a single hotkey — just like NVIDIA ShadowPlay, but open and customizable.

Built with Tauri v2 + React + Windows Graphics Capture + Hardware H.264 encoding.

---

## What's New in v2.3.1

### 🚀 All Resolutions (480p → 4K)
- **480p** (864×480), **720p** (1280×720), **1080p** (1920×1088), **1440p** (2560×1440), **4K** (3840×2160), **Native**
- All dimensions 16-pixel aligned (prevents AMD green macroblock artifacts)
- Rate control configured before encoder output type (Clipsta Lite guardrail #5)
- 3-tier encoder fallback: High profile → Baseline → Bare minimum

### ⚡ Zero-Crash Gameplay Architecture
- **Hotkey saves bypass WebView entirely** — Rust channel (mpsc) → dedicated save-worker thread
- **No WebView events during gameplay** — prevents WebView2 GPU renderer crashes
- **Auto-upload in Rust** — reqwest multipart upload, no JavaScript fetch() with 100MB+ bodies
- **Window starts hidden to tray** — WebView2 never renders during gameplay
- **Warm-start** — D3D11 device pre-created at app launch (saves ~100ms on first recording)
- **Thread join on restart** — prevents encoder session conflicts during resolution changes

### 🎯 Production Stability
- **Single clip per hotkey** — filters key-down only (was double-firing on press+release)
- **spawn_blocking saves** — doesn't block Tauri async runtime
- **catch_unwind on mux** — MF SinkWriter crashes don't kill the app
- **NV12 pool leak fixes** — VP-skip and channel-full paths properly return textures
- **16-pixel alignment on all paths** — including native resolution (rounds up)

### 🔧 Performance
- **Audio buffer pre-allocation** — eliminates 100 heap allocations/sec on audio thread
- **WebView2 cache capped at 50MB** — prevents unbounded disk growth
- **Settings auto-save** — 500ms debounce, no explicit Save button needed
- **Start with Windows** — toggle in settings, uses Windows Registry Run key

---

## Features

### 🎮 Instant Replay Buffer
- **Always-on recording** — captures your screen continuously in the background
- **Hotkey clip saves** — press a key to save the last 30s, 1 min, or 5 min
- **ShadowPlay-style naming** — clips named by game: `Battlefield 6 2026.08.14 - 07.57.44.87.DVR.mp4`
- **All resolutions** — 480p, 720p, 1080p, 1440p, 4K, or native monitor resolution
- **Quality presets** — Standard, High, Ultra with resolution-aware bitrates
- **Minimize to tray** — recording continues when the window is hidden
- **Start with Windows** — launch at login, immediately ready for gaming

### ⚡ GPU-Accelerated Pipeline
- **Windows Graphics Capture (WGC)** — compositor-level capture, minimal game impact
- **Hardware H.264 encoding** — NVENC (NVIDIA), AMF (AMD), or QuickSync (Intel)
- **D3D11 Video Processor** — GPU-accelerated BGRA→NV12 scaling
- **Dedicated encoder thread** — async MFT with backpressure, never blocks capture
- **In-memory ring buffer** — zero-copy clip saves with Arc-wrapped frames
- **Adaptive VP skip** — gracefully degrades under GPU pressure (no game FPS drops)
- **Frame duplication** — maintains constant 60fps output even when WGC misses deliveries
- **3-tier encoder fallback** — High profile → Baseline → bare minimum (works on any GPU)
- **Warm-start** — D3D11 device cached at app launch for instant recording start

### ✂️ Professional Editor
- **Trim** — set IN/OUT points with prominent centered controls
- **Drag-to-Cut** — click and drag to mark sections for removal with live preview
- **Speed Ramping** — mark sections for slow-mo (0.1x–4x) with visual SVG curves
- **Transitions** — Crossfade, Glitch, Whip Pan, Flash, Zoom In/Out at cut points
- **Undo/Redo** — Ctrl+Z / Ctrl+Y with 50-step history
- **Frame-by-frame** — Left/Right arrow keys step ±1 frame
- **Multi-clip timeline** — drag and reorder multiple clips
- **Export presets** — YT Shorts, TikTok, Reels, Square, Original
- **Aspect ratio** — 16:9, 9:16, 1:1, 4:5, 4:3, 21:9 with live preview
- **Video adjustments** — Brightness, Contrast, Saturation
- **NVENC + software fallback** — export works on any GPU
- **Lossless trim** — stream-copy for instant cuts without re-encoding

### 📚 Library
- **Clip browser** — auto-scans your clips folder with game subfolders
- **Thumbnail previews** — extracted video frames with LRU cache
- **Hover preview** — hold over a clip for 1s to see a video popup
- **Search** — filter clips by name
- **Quick actions** — Play, Edit, Upload, Delete, Show in folder
- **Import** — drag & drop or browse files/folders
- **Copy to Downloads** — one-click export

### ☁️ Cloud Upload
- **Mobile pairing** — QR code pairing with companion app
- **Auto-upload** — clips automatically uploaded after save (Rust-native, no WebView)
- **Retry with backoff** — up to 5 retries with exponential delay
- **Upload queue** — progress tracking with background processing
- **Native upload** — entire upload happens in Rust (reqwest multipart), no WebView memory pressure
- **API key secured** — stored in Rust backend binary, never exposed to frontend

### ⚙️ Settings
- **Auto-save** — changes save automatically with 500ms debounce
- **Searchable** — filter settings by keyword
- **Theme** — Dark (default) or OLED Black (pure #000)
- **Hotkeys** — fully customizable global shortcuts
- **Audio** — Desktop + Mic with device selection and live level preview
- **Resolution** — 480p, 720p, 1080p, 1440p, 4K, Native
- **Quality** — Standard, High, Ultra presets with vendor-aware bitrates
- **Buffer duration** — 15s to 5 minutes
- **Start with Windows** — toggle for automatic launch at login
- **Watch folder** — auto-detect new clips from other recorders
- **Minimize to tray** — configurable behavior

### 🎵 Audio
- **WASAPI loopback** — captures system/game audio at 48kHz stereo
- **Microphone mixing** — optional mic input blended with desktop audio
- **Real-time AAC encoding** — pre-encodes during capture for fast saves
- **Pre-allocated buffers** — zero heap allocations on the audio hot path
- **Camera shutter sound** — synthesized 4-layer capture feedback

### 🔒 Security & Stability
- **Restricted CSP** — no `unsafe-eval`, limited `connect-src`
- **Path validation** — all file operations validated against allowed directories
- **No shell execution** — frontend cannot execute arbitrary commands
- **API key backend-only** — cloud API key never reaches the webview
- **Atomic settings writes** — crash-safe temp file + rename pattern
- **catch_unwind on mux** — Media Foundation crashes don't kill the app
- **Thread join on session restart** — prevents resource conflicts
- **NV12 texture free-list** — prevents pool exhaustion over long sessions

---

## Architecture

```
┌─────────────────────────────────────────────────────┐
│  Global Hotkey (Rust)                                │
│  → SAVE_TX channel (no WebView, no JS, no crash)    │
└─────────────────────┬───────────────────────────────┘
                      │
┌─────────────────────▼───────────────────────────────┐
│  Save-Worker Thread (dedicated, always listening)    │
│  → save_clip_standalone()                            │
│  → MF Sink Writer (H.264 passthrough + PCM→AAC)    │
│  → Auto-upload if enabled (reqwest multipart)       │
└─────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────┐
│  WGC Frame Callback (non-blocking)                   │
│  → D3D11 Video Processor (BGRA→NV12, GPU)           │
│  → NV12 free-list allocation                         │
│  → try_send to encoder channel (backpressure)        │
└─────────────────────┬───────────────────────────────┘
                      │
┌─────────────────────▼───────────────────────────────┐
│  Dedicated Encoder Thread                            │
│  → Async MFT GetEvent loop (blocking)               │
│  → ProcessInput (NV12 pool texture)                  │
│  → ProcessOutput (H.264 NAL units)                  │
│  → ring.push_video() with keyframe detection        │
└─────────────────────┬───────────────────────────────┘
                      │
┌─────────────────────▼───────────────────────────────┐
│  EncodedMediaRing (in-memory, Arc-wrapped)           │
│  → VecDeque<EncodedFrame> + keyframe index           │
│  → Bounded by buffer_duration (auto-prune)           │
│  → Buffer pool recycles freed allocations            │
└─────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────┐
│  Audio Capture (WASAPI, event-driven 10ms)           │
│  → Desktop loopback + optional mic mixing            │
│  → PCM ring + real-time AAC encoding                 │
│  → Pre-allocated i16 conversion buffer               │
└─────────────────────────────────────────────────────┘
```

---

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Ctrl+Shift+G` | Save last 30 seconds (default) |
| `F8` | Save last 1 minute (default) |
| `Alt+F10` | Save last 5 minutes (default) |
| `I` | Set trim IN point (editor) |
| `O` | Set trim OUT point (editor) |
| `X` | Quick cut at playhead |
| `S` | Mark speed segment |
| `Space` | Play / Pause |
| `←` / `→` | Step ±1 frame |
| `Ctrl+Z` / `Ctrl+Y` | Undo / Redo |

---

## System Requirements

- **OS:** Windows 10 1903+ / Windows 11
- **GPU:** Any GPU with D3D11 + Video Processor support (NVIDIA, AMD, Intel)
- **Encoder:** NVENC, AMF, QuickSync, or software fallback (libx264)
- **RAM:** 4GB minimum, 8GB+ recommended (ring buffer uses ~865 MB at 1080p/5min)
- **Disk:** SSD recommended for clip saves

---

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Framework | Tauri v2 |
| Frontend | React 19 + TypeScript + Tailwind CSS v4 |
| Backend | Rust |
| Capture | Windows Graphics Capture (WGC) |
| Encoding | Media Foundation Async MFT (Hardware H.264) |
| Scaling | D3D11 Video Processor (BGRA→NV12) |
| Audio | WASAPI Loopback + Capture (48kHz stereo) |
| Muxing | MF Sink Writer (H.264 passthrough + PCM→AAC) |
| Upload | reqwest (blocking multipart, Rust-native) |
| Export | FFmpeg (bundled) with NVENC/libx264 |
| Icons | Lucide React |
| Installer | NSIS |

---

## Build

```bash
# Install dependencies
npm install

# Development
npm run tauri dev

# Production build
npx tauri build
```

Output: `src-tauri/target/release/bundle/nsis/Clipsta_2.3.1_x64-setup.exe`

Requires:
- Node.js 18+
- Rust 1.70+
- Windows SDK (for Media Foundation headers)
- `ffmpeg.exe` in `src-tauri/resources/`

---

## Non-Negotiable Technical Guardrails

These constraints come from real production failures:

1. **One live hardware encoder** — concurrent AMD encoders cause driver crashes
2. **16-pixel aligned dimensions** — 1920×1088, not 1920×1080 (prevents AMD green rows)
3. **Pin VP source/dest rectangles** — NVIDIA otherwise letterboxes
4. **Pre-fill NV12 pool with legal black** — unwritten zero-chroma appears green
5. **Rate control before SetOutputType** — AMD silently ignores later ICodecAPI changes
6. **Configure both bitrate AND VBV buffer** — bitrate alone doesn't reliably cap output
7. **Detect IDR/SPS directly** — don't rely on MFSampleExtension_CleanPoint from AMD
8. **No WebView events during gameplay** — emitting to WebView crashes its GPU renderer
9. **Single hotkey fire** — filter for Pressed state only (plugin fires on both press+release)
10. **spawn_blocking for saves** — synchronous mux blocks Tauri async runtime → WebView death

---

## License

Private — © Clipsta
