# Clipsta Desktop

Gaming clip recorder for Windows — like Nvidia ShadowPlay but open source.

## Features

- **Replay Buffer** — continuously records the last 1–5 minutes in memory
- **One-key clip saving** — press `Alt+F9` to save the last 1 minute, `Alt+F10` for 5 minutes
- **Auto game detection** — picks up fullscreen games automatically  
- **Source picker** — choose any window, display, or game
- **Built-in editor** — trim clips, set in/out points, change aspect ratio, export
- **Library** — browse, preview, and manage all saved clips
- **OBS-style encoding** — software x264 or GPU (NVENC/AMF/QuickSync) via FFmpeg
- **System tray** — keeps running in background, hotkeys work globally

## How to Build

### Requirements
- Node.js v18+ (https://nodejs.org)
- Windows 10/11 x64

### Steps
```
1. Double-click build.bat
   - Installs npm packages
   - Builds Vite + Electron
   - Packages as NSIS installer

2. Output: release/Clipsta Setup 1.0.0.exe
```

Or manually:
```bat
npm install
npm run build:win
```

## Hotkeys (configurable in Settings)

| Action            | Default     |
|-------------------|-------------|
| Start/Stop Record | F9          |
| Save Last 1 Min   | Alt+F9      |
| Save Last 5 Min   | Alt+F10     |

Hotkeys work globally (even when the app is in the tray).

## Export / FFmpeg

The editor exports using FFmpeg. Install FFmpeg and add to PATH:
https://ffmpeg.org/download.html → Windows builds → BtbN releases

## Architecture

```
electron/
  main/index.ts      — Main process: hotkeys, IPC, file I/O, tray
  preload/index.ts   — Secure bridge between main and renderer

src/
  hooks/
    useRecorder.ts   — MediaRecorder + rolling replay buffer
    useSettings.ts   — Persistent settings via electron-store
  components/
    pages/
      CapturePage    — Source picker, live preview, record controls
      LibraryPage    — Clip browser and player
      EditorPage     — Trim timeline, aspect ratio, export
      SettingsPage   — All settings with hotkey capture
```

## Based On
- OBS Studio architecture (replay buffer, encoder selection, game capture concepts)
- Electron + Vite + React + Tailwind
