# Clipsta v2.3

**The gaming clip recorder that just works.** Always recording, always ready. Save the last 30 seconds, 1 minute, or 5 minutes of gameplay with a single hotkey — just like NVIDIA ShadowPlay, but open and customizable.

Built with Tauri v2 + React + Windows Graphics Capture + Hardware H.264 encoding.

---

## Features

### 🎮 Instant Replay Buffer
- **Always-on recording** — captures your screen continuously in the background
- **Hotkey clip saves** — press a key to save the last 30s, 1 min, or 5 min
- **ShadowPlay-style naming** — clips named by game: `Battlefield 6 2026.08.05 - 17.18.49.76.DVR.mp4`
- **720p60 @ 8 Mbps** — matches ShadowPlay quality with BT.709 color metadata
- **Minimize to tray** — recording continues when the window is hidden

### ⚡ GPU-Accelerated Pipeline
- **Windows Graphics Capture (WGC)** — compositor-level capture, zero game impact
- **Hardware H.264 encoding** — NVENC (NVIDIA), AMF (AMD), or QuickSync (Intel)
- **D3D11 Video Processor** — GPU-accelerated BGRA→NV12 scaling
- **Dedicated encoder thread** — async MFT with backpressure, never blocks capture
- **Arc-wrapped ring buffer** — zero-copy clip saves, recycled buffer pool

### ✂️ Professional Editor
- **Trim** — set IN/OUT points with prominent centered controls
- **Drag-to-Cut** — click and drag to mark sections for removal with live preview
  - Draggable cut edges to resize after placement
  - Red diagonal stripe pattern clearly shows cut areas
  - Playback automatically skips cut sections (preview final result)
- **Speed Ramping** — mark sections for slow-mo (0.1x–4x) with visual SVG curves
  - Click ⚡ Speed, drag on timeline to set start/end
  - Draggable speed segment edges
  - Live playback rate control during preview
- **Transitions** — Crossfade, Glitch, Whip Pan, Flash, Zoom In/Out at cut points
  - Hover preview animations on transition buttons
  - Adjustable duration slider
- **Undo/Redo** — Ctrl+Z / Ctrl+Y with 50-step history
- **Frame-by-frame** — Left/Right arrow keys step ±1 frame
- **Thumbnail strip** — video frame previews above the timeline
- **Draggable playhead** — grab and scrub the timeline
- **Multi-clip timeline** — drag and reorder multiple clips
- **Export presets** — YT Shorts, TikTok, Reels, Square, Original
- **Aspect ratio** — 16:9, 9:16, 1:1, 4:5, 4:3, 21:9 with live preview
- **Video adjustments** — Brightness, Contrast, Saturation
- **Export progress bar** — real-time percentage from FFmpeg
- **NVENC + software fallback** — works on any GPU

### 📚 Library
- **Clip browser** — auto-scans your clips folder with game subfolders
- **Thumbnail previews** — extracted video frames for each clip
- **Hover preview** — hold over a clip for 1s to see a video popup
- **Search** — filter clips by name
- **Quick actions** — Play, Edit, Upload, Delete, Show in folder
- **Import** — drag & drop or browse files/folders
- **Copy to Downloads** — one-click export

### ☁️ Cloud Upload
- **Mobile pairing** — QR code pairing with companion app
- **Auto-upload** — new clips automatically queued
- **Retry with backoff** — up to 5 retries with exponential delay
- **Upload queue** — progress tracking, pause, retry individual clips
- **API key secured** — stored in Rust backend, never exposed to frontend

### ⚙️ Settings
- **Searchable** — filter settings by keyword
- **Theme** — Dark (default) or OLED Black (pure #000)
- **Hotkeys** — fully customizable global shortcuts
- **Audio** — Desktop + Mic with device selection
- **Buffer duration** — 30s to 5 minutes
- **Watch folder** — auto-detect new clips from other recorders
- **Minimize to tray** — configurable behavior

### 🎵 Audio
- **WASAPI loopback** — captures system/game audio
- **Microphone mixing** — optional mic input with device selection
- **DSLR camera shutter sound** — satisfying 4-layer capture feedback
- **AAC encoding** — at mux time (no real-time audio encoding overhead)

### 🔒 Security
- **Restricted CSP** — no `unsafe-eval`, limited `connect-src`
- **Path validation** — all file operations validated against allowed directories
- **No shell execution** — frontend cannot execute arbitrary commands
- **API key backend-only** — cloud API key never reaches the webview
- **Atomic settings writes** — crash-safe temp file + rename pattern

---

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `I` | Set trim IN point |
| `O` | Set trim OUT point |
| `X` | Quick cut at playhead (2s) |
| `S` | Mark speed segment start/end |
| `Space` | Play / Pause |
| `←` / `→` | Step back/forward 1 frame |
| `Ctrl+Z` | Undo |
| `Ctrl+Y` | Redo |
| `Esc` | Exit cut/speed mode |

---

## System Requirements

- **OS:** Windows 10 1903+ / Windows 11
- **GPU:** Any GPU with D3D11 support (NVIDIA, AMD, Intel)
- **Encoder:** NVENC, AMF, QuickSync, or software fallback (libx264)
- **RAM:** 4GB minimum, 8GB+ recommended
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
| Audio | WASAPI Loopback + Capture |
| Muxing | MF Sink Writer (H.264 passthrough + AAC) |
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
npm run tauri build
```

Requires:
- Node.js 18+
- Rust 1.70+
- Windows SDK (for Media Foundation headers)
- `ffmpeg.exe` in `src-tauri/resources/`

---

## Architecture

```
┌─────────────────────────────────────────────────┐
│  WGC Frame Callback (non-blocking)              │
│  → D3D11 Video Processor (BGRA→NV12, GPU)      │
│  → try_send to channel (backpressure, cap 4)    │
└──────────────────────┬──────────────────────────┘
                       │
┌──────────────────────▼──────────────────────────┐
│  Dedicated Encoder Thread                        │
│  → Async MFT GetEvent loop                      │
│  → ProcessInput (NV12 texture)                   │
│  → ProcessOutput (H.264 NAL units)              │
│  → Arc::new(data) → ring.push_video()           │
└──────────────────────┬──────────────────────────┘
                       │
┌──────────────────────▼──────────────────────────┐
│  EncodedMediaRing (Arc-wrapped, pool-recycled)   │
│  → VecDeque<EncodedFrame> + keyframe index      │
│  → Prune by duration, recycle buffers           │
└──────────────────────┬──────────────────────────┘
                       │
┌──────────────────────▼──────────────────────────┐
│  save_clip() — on hotkey press                   │
│  → Lock ring (~80μs), Arc::clone slice          │
│  → Unlock → MF Sink Writer (passthrough mux)   │
│  → MP4 file with BT.709 color tags             │
└─────────────────────────────────────────────────┘
```

---

## License

Private — © Clipsta
