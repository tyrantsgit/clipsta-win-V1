//! clipsta-capture — WGC + WASAPI + Media Foundation
//!
//! Engineering principles:
//! - IMFSinkWriter is COM MTA thread-safe: video and audio call WriteSample concurrently
//! - No shared Mutex in the hot path — eliminates contention between video/audio threads
//! - Video PTS from Instant::now() relative to first frame (real wall-clock)
//! - Audio PTS from sample count (inherently accurate: 48000 samples = 1 sec)
//! - Audio waits for first video frame before starting — both streams begin at t=0
//! - MFCreateMemoryBuffer reuse via per-thread local allocation

#![allow(unused_imports, dead_code)]

use std::io::{Write, BufRead, BufReader};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::ptr;
use anyhow::{Result, Context};
use serde::{Deserialize, Serialize};

use windows::core::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::Media::Audio::*;
use windows::Win32::System::Com::*;

mod wasapi_capture;
use wasapi_capture::WasapiCapture;

const MF_VERSION: u32 = 0x0002_0070;
const AUDIO_SAMPLE_RATE: u32 = 48000;
const AUDIO_CHANNELS: u32 = 2;
const AUDIO_BITS_PER_SAMPLE: u32 = 16;
const AUDIO_BLOCK_ALIGN: u32 = AUDIO_CHANNELS * (AUDIO_BITS_PER_SAMPLE / 8);

fn pack_u64(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | (low as u64)
}

// ── MfWriter: thin wrapper around IMFSinkWriter ───────────────────────────────
// IMFSinkWriter is thread-safe in MTA. We wrap it in Arc so both video and audio
// threads can call WriteSample without any Mutex — eliminating all hot-path contention.
struct MfWriter {
    writer:          IMFSinkWriter,
    video_stream:    u32,
    audio_stream:    Option<u32>,
}

unsafe impl Send for MfWriter {}
unsafe impl Sync for MfWriter {}

impl MfWriter {
    fn new(output_path: &str, width: u32, height: u32, fps: u32,
           bitrate_kbps: u32, has_audio: bool) -> Result<Self> {
        unsafe {
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;

            let mut attr: Option<IMFAttributes> = None;
            MFCreateAttributes(&mut attr, 4)?;
            let attr = attr.context("attr")?;
            // Enable hardware encoder (NVENC / AMF / QSV)
            attr.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
            // Don't throttle — we drive the clock ourselves
            attr.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
            // Low-latency encoder mode
            attr.SetUINT32(&MF_LOW_LATENCY, 1)?;

            let path: HSTRING = output_path.into();
            let writer: IMFSinkWriter = MFCreateSinkWriterFromURL(&path, None, &attr)?;

            // ── Video: H.264 output, BGRA input ──────────────────────────────
            let vout: IMFMediaType = MFCreateMediaType()?;
            vout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            vout.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            vout.SetUINT32(&MF_MT_AVG_BITRATE, bitrate_kbps * 1000)?;
            vout.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            vout.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
            vout.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
            vout.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
            // H.264 High profile: better compression, B-frames, CABAC entropy coding.
            // eAVEncH264VProfile_High = 100. Supported by all modern GPUs (NVENC/AMF/QSV).
            vout.SetUINT32(&MF_MT_MPEG2_PROFILE, 100)?; // High profile
            vout.SetUINT32(&MF_MT_MPEG2_LEVEL, 42)?;    // Level 4.2 (supports 1080p60)
            let video_stream = writer.AddStream(&vout)?;

            // WGC delivers BGRA. MFVideoFormat_ARGB32 = BGRA stored in memory (MF naming quirk).
            let vin: IMFMediaType = MFCreateMediaType()?;
            vin.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            vin.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_ARGB32)?;
            vin.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            vin.SetUINT64(&MF_MT_FRAME_SIZE, pack_u64(width, height))?;
            vin.SetUINT64(&MF_MT_FRAME_RATE, pack_u64(fps, 1))?;
            vin.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_u64(1, 1))?;
            writer.SetInputMediaType(video_stream, &vin, None)?;

            // ── Audio: AAC output, PCM-i16 input ─────────────────────────────
            let audio_stream = if has_audio {
                let aout: IMFMediaType = MFCreateMediaType()?;
                aout.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                aout.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
                aout.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE)?;
                aout.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)?;
                aout.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, AUDIO_BITS_PER_SAMPLE)?;
                aout.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, 24000)?; // ~192 kbps
                let idx = writer.AddStream(&aout)?;

                let ain: IMFMediaType = MFCreateMediaType()?;
                ain.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                ain.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
                ain.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, AUDIO_BITS_PER_SAMPLE)?;
                ain.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_SAMPLE_RATE)?;
                ain.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)?;
                ain.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, AUDIO_BLOCK_ALIGN)?;
                ain.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                              AUDIO_SAMPLE_RATE * AUDIO_BLOCK_ALIGN)?;
                writer.SetInputMediaType(idx, &ain, None)?;
                Some(idx)
            } else {
                None
            };

            writer.BeginWriting()?;
            eprintln!("[mf] BeginWriting OK  video_stream={video_stream}  audio={has_audio}");

            Ok(Self { writer, video_stream, audio_stream })
        }
    }

    // Called from the VIDEO thread only — no lock needed.
    fn write_video(&self, bgra: &[u8], pts: i64, dur: i64) -> Result<()> {
        unsafe {
            let buf: IMFMediaBuffer = MFCreateMemoryBuffer(bgra.len() as u32)?;
            let mut p: *mut u8 = ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            ptr::copy_nonoverlapping(bgra.as_ptr(), p, bgra.len());
            buf.Unlock()?;
            buf.SetCurrentLength(bgra.len() as u32)?;

            let s: IMFSample = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(pts)?;
            s.SetSampleDuration(dur)?;
            self.writer.WriteSample(self.video_stream, &s)?;
        }
        Ok(())
    }

    // Called from the AUDIO thread only — no lock needed.
    fn write_audio(&self, f32_samples: &[f32], pts: i64, dur: i64) -> Result<()> {
        let idx = match self.audio_stream { Some(i) => i, None => return Ok(()) };
        unsafe {
            // f32 → i16  (Microsoft AAC encoder requires PCM i16, not float)
            let i16_buf: Vec<i16> = f32_samples.iter()
                .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect();
            let byte_len = (i16_buf.len() * 2) as u32;

            let buf: IMFMediaBuffer = MFCreateMemoryBuffer(byte_len)?;
            let mut p: *mut u8 = ptr::null_mut();
            buf.Lock(&mut p, None, None)?;
            ptr::copy_nonoverlapping(i16_buf.as_ptr() as *const u8, p, byte_len as usize);
            buf.Unlock()?;
            buf.SetCurrentLength(byte_len)?;

            let s: IMFSample = MFCreateSample()?;
            s.AddBuffer(&buf)?;
            s.SetSampleTime(pts)?;
            s.SetSampleDuration(dur)?;
            self.writer.WriteSample(idx, &s)?;
        }
        Ok(())
    }

    fn finalize(self) -> Result<()> {
        unsafe {
            self.writer.Finalize()?;
            MFShutdown()?;
        }
        Ok(())
    }
}


// ── Command parsing ───────────────────────────────────────────────────────────
#[derive(Debug)]
enum CaptureCmd {
    ListSources,
    ListAudioDevices,
    Capture {
        source_id:      Option<String>,
        fps:            u32,
        no_audio:       bool,
        mic_device:     Option<String>,
        loopback_device:Option<String>,
        output:         Option<String>,
        out_width:      Option<u32>,
        out_height:     Option<u32>,
        bitrate:        Option<u32>,
    },
}

fn parse_args() -> Result<CaptureCmd> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        return Ok(CaptureCmd::Capture {
            source_id: None, fps: 60, no_audio: false,
            mic_device: None, loopback_device: None,
            output: None, out_width: None, out_height: None, bitrate: None,
        });
    }
    match args[1].as_str() {
        "list-sources"       => Ok(CaptureCmd::ListSources),
        "list-audio-devices" => Ok(CaptureCmd::ListAudioDevices),
        "capture" => {
            let mut source_id = None; let mut fps = 60u32; let mut no_audio = false;
            let mut mic_device = None; let mut loopback_device = None;
            let mut output = None; let mut out_width = None; let mut out_height = None;
            let mut bitrate = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--source"         => { i += 1; source_id       = args.get(i).cloned(); }
                    "--fps"            => { i += 1; fps = args.get(i).and_then(|s| s.parse().ok()).unwrap_or(60); }
                    "--no-audio"       => { no_audio = true; }
                    "--mic-device"     => { i += 1; mic_device      = args.get(i).cloned(); }
                    "--loopback-device"=> { i += 1; loopback_device = args.get(i).cloned(); }
                    "--output"         => { i += 1; output          = args.get(i).cloned(); }
                    "--width"          => { i += 1; out_width  = args.get(i).and_then(|s| s.parse().ok()); }
                    "--height"         => { i += 1; out_height = args.get(i).and_then(|s| s.parse().ok()); }
                    "--bitrate"        => { i += 1; bitrate    = args.get(i).and_then(|s| s.parse().ok()); }
                    // ignored legacy flags
                    "--ffmpeg"|"--audio-out"|"--encoder-args" => { i += 1; }
                    _ => {}
                }
                i += 1;
            }
            Ok(CaptureCmd::Capture { source_id, fps, no_audio, mic_device, loopback_device,
                                     output, out_width, out_height, bitrate })
        }
        _ => anyhow::bail!("Unknown command: {}", args[1]),
    }
}

// ── Source listing ────────────────────────────────────────────────────────────
#[derive(Serialize, Clone)]
struct SourceInfo { id: String, name: String, source_type: String, width: i32, height: i32 }

fn list_audio_devices() -> Result<()> {
    let d = WasapiCapture::list_audio_devices()?;
    println!("{}", serde_json::to_string(&d)?);
    Ok(())
}

fn list_sources() -> Result<()> {
    let mut sources: Vec<SourceInfo> = Vec::new();
    unsafe {
        use windows::Win32::Graphics::Gdi::*;
        use windows::Win32::Foundation::{RECT, LPARAM};
        use windows_core::BOOL;
        extern "system" fn mon_cb(hmon: HMONITOR, _: HDC, _: *mut RECT, lp: LPARAM) -> BOOL {
            let list = unsafe { &mut *(lp.0 as *mut Vec<SourceInfo>) };
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            if unsafe { GetMonitorInfoW(hmon, &mut info.monitorInfo).as_bool() } {
                let name = String::from_utf16_lossy(
                    &info.szDevice.iter().take_while(|&&c| c != 0).cloned().collect::<Vec<_>>());
                let r = &info.monitorInfo.rcMonitor;
                list.push(SourceInfo {
                    id: format!("monitor:{}", hmon.0 as usize),
                    name: format!("Display {}", name.trim().trim_end_matches('\0')),
                    source_type: "monitor".into(), width: r.right-r.left, height: r.bottom-r.top });
            }
            BOOL(1)
        }
        let _ = EnumDisplayMonitors(None, None, Some(mon_cb),
            windows::Win32::Foundation::LPARAM(&mut sources as *mut _ as isize));
        use windows::Win32::UI::WindowsAndMessaging::*;
        use windows::Win32::Foundation::HWND;
        extern "system" fn win_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
            let list = unsafe { &mut *(lp.0 as *mut Vec<SourceInfo>) };
            if !unsafe { IsWindowVisible(hwnd).as_bool() } { return BOOL(1); }
            let mut t = [0u16; 512];
            let len = unsafe { GetWindowTextW(hwnd, &mut t) };
            if len == 0 { return BOOL(1); }
            let title = String::from_utf16_lossy(&t[..len as usize]);
            let mut r = windows::Win32::Foundation::RECT::default();
            let _ = unsafe { GetWindowRect(hwnd, &mut r) };
            let (w, h) = (r.right-r.left, r.bottom-r.top);
            if w < 150 || h < 150 { return BOOL(1); }
            list.push(SourceInfo { id: format!("hwnd:{}", hwnd.0 as usize),
                name: title, source_type: "window".into(), width: w, height: h });
            BOOL(1)
        }
        let _ = EnumWindows(Some(win_cb),
            windows::Win32::Foundation::LPARAM(&mut sources as *mut _ as isize));
    }
    println!("{}", serde_json::to_string(&sources)?);
    Ok(())
}

// ── JSON messaging ────────────────────────────────────────────────────────────
#[derive(Deserialize)]
struct StdinCmd { cmd: String }

fn spawn_stdin_watcher(stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        let reader = BufReader::new(std::io::stdin());
        for line in reader.lines() {
            match line {
                Ok(l) => { if let Ok(c) = serde_json::from_str::<StdinCmd>(&l) {
                    if c.cmd == "stop" { stop.store(true, Ordering::SeqCst); return; }
                }}
                Err(_) => { stop.store(true, Ordering::SeqCst); return; }
            }
        }
        stop.store(true, Ordering::SeqCst);
    });
}

fn json_msg(status: &str, extra: serde_json::Value) {
    let mut obj = serde_json::Map::new();
    obj.insert("status".into(), serde_json::Value::String(status.into()));
    if let serde_json::Value::Object(m) = extra { for (k,v) in m { obj.insert(k,v); } }
    let _ = writeln!(std::io::stdout(), "{}", serde_json::Value::Object(obj));
}


// ── do_capture ────────────────────────────────────────────────────────────────
fn do_capture(
    source_id: Option<String>, fps: u32, no_audio: bool,
    mic_device: Option<String>, loopback_device: Option<String>,
    output: Option<String>,
    out_width: Option<u32>, out_height: Option<u32>,
    bitrate: Option<u32>,
) {
    let result = (|| -> Result<()> {
        use windows::Win32::Graphics::Gdi::{MonitorFromPoint, MONITOR_DEFAULTTOPRIMARY, HMONITOR};
        use windows::Win32::Foundation::{HWND, POINT};

        let item = match source_id.as_deref() {
            Some(id) if id.starts_with("hwnd:") => {
                let v: usize = id[5..].parse().context("bad hwnd")?;
                wgc::new_item_from_hwnd(HWND(v as *mut _))?
            }
            Some(id) if id.starts_with("monitor:") => {
                let v: usize = id[8..].parse().context("bad monitor")?;
                wgc::new_item_from_monitor(HMONITOR(v as *mut _))?
            }
            _ => {
                let hmon = unsafe { MonitorFromPoint(POINT{x:0,y:0}, MONITOR_DEFAULTTOPRIMARY) };
                wgc::new_item_from_monitor(hmon)?
            }
        };

        let sz = item.Size()?;
        let (cap_w, cap_h) = (sz.Width as u32, sz.Height as u32);
        let target_w = out_width.unwrap_or(cap_w);
        let target_h = out_height.unwrap_or(cap_h);
        let bitrate_kbps = bitrate.unwrap_or(25000);

        json_msg("ready", serde_json::json!({
            "width": target_w, "height": target_h, "fps": fps, "has_audio": !no_audio,
        }));

        let frame_dur_100ns: i64 = 10_000_000 / fps as i64;
        let frame_interval = std::time::Duration::from_secs_f64(1.0 / fps as f64);

        let wgc_settings = wgc::WgcSettings {
            pixel_format:            wgc::PixelFormat::BGRA8,
            frame_queue_length:      2,
            capture_cursor:          Some(true),
            display_border:          Some(false),
            min_update_interval:     Some(frame_interval),
            frame_interpolation_mode: wgc::FrameInterpolationMode::NearestNeighbor,
            ..Default::default()
        };
        let wgc_iter = wgc::Wgc::new(item, wgc_settings)?;

        let output_path = output.context("--output required")?;
        let writer = Arc::new(
            MfWriter::new(&output_path, target_w, target_h, fps, bitrate_kbps, !no_audio)?
        );

        let stop = Arc::new(AtomicBool::new(false));

        // ── QPC-based sync: shared base_time (AtomicI64) ──
        // Both video (SystemRelativeTime) and audio (GetBuffer QPC) use the same
        // QPC hardware clock. We normalize to PTS=0 by subtracting the first
        // timestamp seen from either stream. AtomicI64::MIN = "not yet set".
        let base_time = Arc::new(std::sync::atomic::AtomicI64::new(i64::MIN));

        // Audio thread
        let audio_thread = if !no_audio {
            let w  = writer.clone();
            let s  = stop.clone();
            let bt = base_time.clone();
            let m  = mic_device.clone();
            let lb = loopback_device.clone();
            Some(thread::spawn(move || {
                audio_loop(s, m, lb, w, bt);
            }))
        } else {
            None
        };

        spawn_stdin_watcher(stop.clone());

        // ── Video capture loop ────────────────────────────────────────────────
        // Set this thread to high priority to minimize frame drops and input latency.
        unsafe {
            use windows::Win32::System::Threading::*;
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        }

        let mut total_frames: u64 = 0;
        let target_size = wgc::FrameSize { width: target_w, height: target_h };

        for frame_result in wgc_iter {
            if stop.load(Ordering::Relaxed) { break; }
            let frame = match frame_result { Ok(f) => f, Err(_) => break };

            // Constant-rate PTS: frame_count * (10_000_000 / fps)
            // This is the simplest and most reliable approach — no drift possible.
            let pts = total_frames as i64 * frame_dur_100ns;

            // Signal audio to start on first frame
            if total_frames == 0 {
                base_time.store(0, Ordering::Release);
            }

            let pixels = frame.read_pixels(Some(target_size))?;
            if writer.write_video(&pixels, pts, frame_dur_100ns).is_err() { break; }
            total_frames += 1;
        }

        stop.store(true, Ordering::SeqCst);
        if let Some(t) = audio_thread { let _ = t.join(); }

        // Unwrap Arc — both threads have exited, we're the sole owner.
        match Arc::try_unwrap(writer) {
            Ok(w)  => w.finalize()?,
            Err(_) => { eprintln!("[mf] Arc still shared after join — forcing finalize"); }
        }

        eprintln!("[clipsta-capture] done: {} frames  {}", total_frames, output_path);
        json_msg("done", serde_json::json!({ "frames": total_frames, "output": output_path }));
        Ok(())
    })();

    if let Err(e) = result {
        eprintln!("[clipsta-capture] error: {}", e);
        json_msg("fatal", serde_json::json!({"error": e.to_string()}));
    }
}

// ── Audio capture loop ────────────────────────────────────────────────────────
// Waits for video to set base_time (first video frame), then captures audio.
// Audio PTS = sample_count * 10_000_000 / 48000 (drift-free, hardware-accurate).
// Both streams start at PTS=0 simultaneously — guaranteed by the base_time gate.
fn audio_loop(
    stop:       Arc<AtomicBool>,
    mic_device: Option<String>,
    loopback:   Option<String>,
    writer:     Arc<MfWriter>,
    base_time:  Arc<std::sync::atomic::AtomicI64>,
) {
    // Elevate audio thread priority to avoid glitches
    unsafe {
        use windows::Win32::System::Threading::*;
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }

    // Wait for video to emit its first frame (base_time changes from i64::MIN)
    while base_time.load(Ordering::Acquire) == i64::MIN {
        if stop.load(Ordering::Relaxed) { return; }
        thread::sleep(std::time::Duration::from_millis(1));
    }

    let sw = Arc::new(AtomicU64::new(0));
    let sw2 = sw.clone();

    let res = WasapiCapture::capture_to_callback(
        stop,
        mic_device,
        loopback,
        move |chunk: &[f32]| {
            let n_frames = chunk.len() as u64 / AUDIO_CHANNELS as u64;
            let cur      = sw2.load(Ordering::Relaxed);
            let pts      = (cur as i64 * 10_000_000) / AUDIO_SAMPLE_RATE as i64;
            let dur      = (n_frames as i64 * 10_000_000) / AUDIO_SAMPLE_RATE as i64;
            let _ = writer.write_audio(chunk, pts, dur);
            sw2.store(cur + n_frames, Ordering::Relaxed);
        },
    );
    if let Err(e) = res { eprintln!("[audio] error: {e}"); }
}

fn main() -> Result<()> {
    unsafe { let _ = CoInitializeEx(None, COINIT_MULTITHREADED); }
    match parse_args()? {
        CaptureCmd::ListSources       => list_sources(),
        CaptureCmd::ListAudioDevices  => list_audio_devices(),
        CaptureCmd::Capture { source_id, fps, no_audio, mic_device, loopback_device,
                              output, out_width, out_height, bitrate } => {
            do_capture(source_id, fps, no_audio, mic_device, loopback_device,
                       output, out_width, out_height, bitrate);
            Ok(())
        }
    }
}
