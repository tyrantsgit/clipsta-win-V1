//! WASAPI desktop loopback + mic audio capture
//!
//! Writes raw f32 LE interleaved stereo PCM directly to a file at the
//! native loopback sample rate. If a mic device is specified, both
//! loopback and mic audio are mixed together (sample-summed, hard-clipped)
//! and written as a single f32 stereo stream.
//!
//! The output path is passed via --audio-out at startup. Writing directly
//! to a file avoids pipe-buffer bottlenecks between Rust and Node.js.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use windows::Win32::Foundation::{CloseHandle, PROPERTYKEY, WAIT_OBJECT_0, HANDLE};
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, DEVICE_STATE, IAudioCaptureClient, IAudioClient, IMMDevice,
    IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED,
    AUDCLNT_STREAMFLAGS_LOOPBACK, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, WAVEFORMATEX,
};
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Wrapper that asserts Send for Windows COM types.
/// Safe because COM objects in MTA are thread-safe, and HANDLEs are kernel handles.
struct SafeHandle {
    client: IAudioClient,
    capture: IAudioCaptureClient,
    event: HANDLE,
}
unsafe impl Send for SafeHandle {}
unsafe impl Sync for SafeHandle {}

/// Shared audio sample accumulator.
struct AudioMixer {
    loopback: VecDeque<f32>,
    mic: VecDeque<f32>,
    mic_active: bool,
}

fn write_audio_raw(samples_f32_le: &[u8], file: &mut Option<std::fs::File>) -> Result<()> {
    if let Some(ref mut f) = file {
        f.write_all(samples_f32_le)?;
    } else {
        let mut err = std::io::stderr();
        err.write_all(samples_f32_le)?;
    }
    Ok(())
}

fn clip_sample(s: f32) -> f32 {
    s.clamp(-1.0, 1.0)
}

fn debug_timestamp() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

macro_rules! read_wfx {
    ($ptr:expr, $field:ident) => {
        std::ptr::addr_of!((*$ptr).$field).read_unaligned()
    };
}

/// Format info parsed from WAVEFORMATEX.
#[derive(Clone, Copy)]
struct AudioFormat {
    channels: usize,
    bps: usize,
    sample_rate: u32,
    is_float: bool,
}

fn parse_format(ptr: *const WAVEFORMATEX) -> AudioFormat {
    unsafe {
        AudioFormat {
            channels: read_wfx!(ptr, nChannels) as usize,
            bps: read_wfx!(ptr, wBitsPerSample) as usize,
            sample_rate: read_wfx!(ptr, nSamplesPerSec),
            is_float: {
                let tag = read_wfx!(ptr, wFormatTag);
                tag == 3 || tag == 0xFFFE
            },
        }
    }
}

pub struct WasapiCapture;

impl WasapiCapture {
    /// Run WASAPI capture loop. Writes mixed f32 stereo PCM to audio_out file,
    /// or stderr if audio_out is None (backward compat).
    /// If `pipe_file` is Some, it is used as the output (pre-opened named pipe handle),
    /// otherwise audio_out path is opened as a regular file.
    pub fn capture_loop(stop: Arc<AtomicBool>, mic_device: Option<String>, loopback_device: Option<String>, audio_out: Option<String>, pipe_file: Option<std::fs::File>) -> Result<()> {
        // Debug log to file (avoids corrupting PCM stream on stderr)
        let debug_path = std::env::temp_dir().join("clipsta_audio_debug.txt");
        let mut debug_log = || -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path)?;
            writeln!(f, "[{}] capture_loop: mic={:?} loopback={:?}", debug_timestamp(), mic_device, loopback_device)
        };
        let _ = debug_log();

        unsafe {
            // STA threads co-initialize implicitly; MTA threads must call
            // CoInitializeEx before any COM call (the main thread calls it,
            // but spawned threads do NOT inherit COM state).
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] CoInitializeEx OK", debug_timestamp())
            });
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("IMMDeviceEnumerator")?;
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] IMMDeviceEnumerator OK", debug_timestamp())
            });

            let render_device = if let Some(ref id) = loopback_device {
                if id.is_empty() {
                    enumerator.GetDefaultAudioEndpoint(eRender, eConsole).context("GetDefaultAudioEndpoint")?
                } else {
                    match Self::find_render_device(&enumerator, id) {
                        Ok(dev) => dev,
                        Err(e) => {
                            let msg = format!("find_render_device failed for '{}': {} — falling back to default audio endpoint", &id[..id.len().min(32)], e);
                            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                                use std::io::Write;
                                writeln!(f, "[{}] {}", debug_timestamp(), msg)
                            });
                            enumerator.GetDefaultAudioEndpoint(eRender, eConsole).context("GetDefaultAudioEndpoint fallback")?
                        }
                    }
                }
            } else {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole).context("GetDefaultAudioEndpoint")?
            };
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] GetDefaultAudioEndpoint OK", debug_timestamp())
            });

            let lb_client: IAudioClient = render_device.Activate(CLSCTX_ALL, None)
                .context("Activate loopback")?;
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] Activate loopback OK", debug_timestamp())
            });
            let lb_fmt = lb_client.GetMixFormat().context("GetMixFormat")? as *const WAVEFORMATEX;
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] GetMixFormat OK", debug_timestamp())
            });
			lb_client.Initialize(
                AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                2_000_000, 0, lb_fmt, None,
            ).context("Initialize loopback")?;
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] Initialize loopback OK", debug_timestamp())
            });
            let lb_capture: IAudioCaptureClient = lb_client.GetService()
                .context("GetService loopback")?;

            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] loopback initialized OK", debug_timestamp())
            });

            let fmt = parse_format(lb_fmt);
            let bpf = fmt.channels * (fmt.bps / 8);

            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] fmt={}ch {}Hz {}bps float={}", debug_timestamp(), fmt.channels, fmt.sample_rate, fmt.bps, fmt.is_float)
            });

            // ── Optional mic setup ────────────────────────────────────────
            let (mic_client, mic_capture, mic_fmt, mic_bpf) = if let Some(ref id) = mic_device {
                if id.is_empty() {
                    (None, None, None, 0)
                } else {
                    match Self::init_mic(&enumerator, id) {
                        Ok((c, cap, ptr)) => {
                            let mfmt = parse_format(ptr);
                            let mbpf = mfmt.channels * (mfmt.bps / 8);
                            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                                use std::io::Write;
                                writeln!(f, "[{}] mic initialized OK fmt={}ch {}Hz {}bps", debug_timestamp(), mfmt.channels, mfmt.sample_rate, mfmt.bps)
                            });
                            (Some(c), Some(cap), Some(mfmt), mbpf)
                        }
                        Err(e) => {
                            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                                use std::io::Write;
                                writeln!(f, "[{}] mic init FAILED: {}", debug_timestamp(), e)
                            });
                            (None, None, None, 0)
                        }
                    }
                }
            } else {
                (None, None, None, 0)
            };

            let mixer = Arc::new(Mutex::new(AudioMixer {
                loopback: VecDeque::new(),
                mic: VecDeque::new(),
                mic_active: mic_client.is_some(),
            }));

            // ── Events ────────────────────────────────────────────────────
            let lb_event = CreateEventW(None, false, false, None).context("CreateEventW")?;
            lb_client.SetEventHandle(lb_event).context("SetEventHandle")?;
            lb_client.Start().context("Loopback Start")?;
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] loopback Start() OK", debug_timestamp())
            });

            let mic_event = mic_client.as_ref().and_then(|mc| {
                let e = CreateEventW(None, false, false, None).ok()?;
                mc.SetEventHandle(e).ok()?;
                mc.Start().ok()?;
                Some(e)
            });

            // ── Create SafeHandles for thread-safe sharing ──────────────────
            let lb_safe = Arc::new(SafeHandle {
                client: lb_client,
                capture: lb_capture,
                event: lb_event,
            });
            let mic_safe = mic_client.zip(mic_capture).zip(mic_event).map(|((mc, mcap), me)| {
                Arc::new(SafeHandle { client: mc, capture: mcap, event: me })
            });

            // mixer and stop are already Arc from the caller

            // ── Spawn threads with owned Arcs ─────────────────────────────
            let target_sr = fmt.sample_rate;
            let mic_active = mic_fmt.is_some();

            // Writer thread: mix and output to file (or stderr fallback)
            let writer_handle = {
                let w_mixer = mixer.clone();
                let w_stop = stop.clone();
                let w_audio_out = audio_out.clone();
                let chunk_frames = (target_sr as usize / 20).max(512);
                let w_debug_path = debug_path.clone();
                let w_pipe_file = pipe_file;
                std::thread::Builder::new().name("audio-writer".into()).spawn(move || {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&w_debug_path).and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "[{}] writer thread started, chunk_frames={} audio_out={:?} has_pipe_file={}", debug_timestamp(), chunk_frames, w_audio_out, w_pipe_file.is_some())
                    });
                    // Use pipe_file (named pipe) when provided, otherwise fall back to audio_out file
                    let mut out_file: Option<std::fs::File> = if let Some(pf) = w_pipe_file {
                        Some(pf)
                    } else {
                        w_audio_out.and_then(|p| std::fs::File::create(p).ok())
                    };
                    let mut written_bytes: u64 = 0;
                    loop {
                        let should_stop = w_stop.load(Ordering::SeqCst);
                        if should_stop {
                            // Small delay to let capture threads push their final data
                            std::thread::sleep(std::time::Duration::from_millis(200));
                            let m = w_mixer.lock().unwrap();
                            if m.loopback.is_empty() && (!mic_active || m.mic.is_empty()) {
                                break;
                            }
                        }
                        let out = {
                            let mut m = w_mixer.lock().unwrap();
                            if mic_active {
                                let n = m.loopback.len().min(m.mic.len()).min(chunk_frames * 2);
                                if n < 64 { continue; }
                                (0..n).map(|_| {
                                    clip_sample(
                                        m.loopback.pop_front().unwrap_or(0.0)
                                        + m.mic.pop_front().unwrap_or(0.0),
                                    )
                                }).collect::<Vec<_>>()
                            } else {
                                let n = m.loopback.len().min(chunk_frames * 2);
                                if n < 64 { continue; }
                                m.loopback.drain(..n).collect::<Vec<_>>()
                            }
                        };
                        let mut bytes = Vec::with_capacity(out.len() * 4);
                        for s in &out {
                            bytes.extend_from_slice(&s.to_le_bytes());
                        }
                        written_bytes += bytes.len() as u64;
                        if written_bytes % (48000 * 4 * 2) == 0 {
                            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&w_debug_path).and_then(|mut f| {
                                use std::io::Write;
                                writeln!(f, "[{}] writer: {} bytes written so far", debug_timestamp(), written_bytes)
                            });
                        }
                        if write_audio_raw(&bytes, &mut out_file).is_err() { break; }
                    }
                    if let Some(f) = out_file.as_ref() {
                        let _ = f.sync_all();
                    }
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&w_debug_path).and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "[{}] writer thread exiting, total bytes={}", debug_timestamp(), written_bytes)
                    });
                }).expect("writer thread")
            };

            // Loopback capture thread
            let loopback_handle = {
                let lb = lb_safe.clone();
                let m = mixer.clone();
                let s = stop.clone();
                let lb_debug_path = debug_path.clone();
                std::thread::Builder::new().name("audio-loopback".into()).spawn(move || {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&lb_debug_path).and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "[{}] loopback capture thread started", debug_timestamp())
                    });
                    loopback_capture_loop(&lb.client, &lb.capture, lb.event, &s, &m, fmt, bpf);
                }).expect("loopback thread")
            };

            // Mic capture thread (if enabled)
            let mic_handle = if let Some(ref ms) = mic_safe {
                let ms_clone = ms.clone();
                let m = mixer.clone();
                let s = stop.clone();
                let mfmt = mic_fmt.unwrap();
                let mic_debug_path = debug_path.clone();
                Some(std::thread::Builder::new().name("audio-mic".into()).spawn(move || {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&mic_debug_path).and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "[{}] mic capture thread started", debug_timestamp())
                    });
                    mic_capture_loop(&ms_clone.client, &ms_clone.capture, ms_clone.event, &s, &m, mfmt, mic_bpf, target_sr);
                }).expect("mic thread"))
            } else {
                None
            };

            // ── Wait for stop signal ──────────────────────────────────────
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] capture_loop waiting for stop signal", debug_timestamp())
            });
            while !stop.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] capture_loop stop signal received, joining threads", debug_timestamp())
            });

            // Clean up WASAPI clients (signals capture threads to stop getting data)
            let _ = lb_safe.client.Stop();
            CloseHandle(lb_safe.event).ok();
            if let Some(ref ms) = mic_safe {
                let _ = ms.client.Stop();
                CloseHandle(ms.event).ok();
            }

            // Join all threads to ensure pipe_file is properly closed before returning
            // This is critical: the writer thread holds the named pipe handle that FFmpeg reads from.
            // We must wait for it to finish writing and drop the handle so FFmpeg gets EOF.
            let _ = loopback_handle.join();
            if let Some(mh) = mic_handle { let _ = mh.join(); }
            let _ = writer_handle.join();

            let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                use std::io::Write;
                writeln!(f, "[{}] capture_loop all threads joined, returning", debug_timestamp())
            });
        }
        Ok(())
    }

    unsafe fn find_render_device(
        enumerator: &IMMDeviceEnumerator,
        device_id: &str,
    ) -> Result<IMMDevice> {
        let collection = enumerator
            .EnumAudioEndpoints(eRender, DEVICE_STATE(1))
            .context("EnumAudioEndpoints render")?;
        let count = collection.GetCount()?;
        for i in 0..count {
            let dev = collection.Item(i)?;
            let id_str = dev.GetId()
                .map(|id| id.to_string().unwrap_or_default())
                .unwrap_or_default();
            if id_str.contains(device_id) || device_id.contains(&id_str) {
                return Ok(dev);
            }
        }
        anyhow::bail!("render device not found matching: {}", device_id)
    }

	unsafe fn init_mic(
		enumerator: &IMMDeviceEnumerator,
		device_id: &str,
	) -> Result<(IAudioClient, IAudioCaptureClient, *const WAVEFORMATEX)> {
		let device = if device_id == "default" {
			enumerator.GetDefaultAudioEndpoint(eCapture, eConsole).context("GetDefaultAudioEndpoint mic")?
		} else {
			let collection = enumerator
				.EnumAudioEndpoints(eCapture, DEVICE_STATE(1))
				.context("EnumAudioEndpoints")?;
			let count = collection.GetCount()?;
			let mut found: Option<IMMDevice> = None;
			for i in 0..count {
				let dev = collection.Item(i)?;
				let id_str = dev.GetId()
					.map(|id| id.to_string().unwrap_or_default())
					.unwrap_or_default();
				if id_str.contains(device_id) || device_id.contains(&id_str) {
					found = Some(dev);
					break;
				}
			}
			match found {
				Some(dev) => dev,
				None => {
					let debug_path = std::env::temp_dir().join("clipsta_audio_debug.txt");
					let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
						use std::io::Write;
						writeln!(f, "[{}] init_mic: '{}' not found, falling back to default capture endpoint", debug_timestamp(), device_id)
					});
					enumerator.GetDefaultAudioEndpoint(eCapture, eConsole).context("GetDefaultAudioEndpoint fallback")?
				}
			}
		};
		let c: IAudioClient = device.Activate(CLSCTX_ALL, None).context("Mic Activate")?;
		let fmt = c.GetMixFormat().context("Mic GetMixFormat")? as *const WAVEFORMATEX;
		c.Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_EVENTCALLBACK, 2_000_000, 0, fmt, None)
			.context("Mic Initialize")?;
		let cap: IAudioCaptureClient = c.GetService().context("Mic GetService")?;
		Ok((c, cap, fmt))
	}

    pub fn get_sample_rate(loopback_device: Option<String>) -> u32 {
        unsafe {
            let Ok(en): std::result::Result<IMMDeviceEnumerator, _> =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
            else { return 48000; };
            let dev = match loopback_device {
                Some(ref id) if !id.is_empty() => {
                    match Self::find_render_device(&en, id) {
                        Ok(d) => d,
                        Err(_) => {
                            let Ok(d) = en.GetDefaultAudioEndpoint(eRender, eConsole) else { return 48000; };
                            d
                        }
                    }
                }
                _ => {
                    let Ok(d) = en.GetDefaultAudioEndpoint(eRender, eConsole) else { return 48000; };
                    d
                }
            };
            let Ok(ac): std::result::Result<IAudioClient, _> = dev.Activate(CLSCTX_ALL, None)
            else { return 48000; };
            let Ok(fmt) = ac.GetMixFormat() else { return 48000; };
            parse_format(fmt as *const WAVEFORMATEX).sample_rate
        }
    }

    /// Enumerate all WASAPI audio devices (render/output + capture/input).
    /// Returns JSON-serializable array of { id, kind } objects.
    /// The id is the raw WASAPI persistent device ID, which is what
    /// --mic-device and --loopback-device expect for exact matching.
    /// Capture audio and send chunks to a callback function.
    /// This is used with Media Foundation where we write directly to the MFSinkWriter.
    /// The callback receives interleaved f32 stereo samples.
    pub fn capture_to_callback<F>(
        stop: Arc<AtomicBool>,
        mic_device: Option<String>,
        loopback_device: Option<String>,
        callback: F,
    ) -> Result<()>
    where
        F: Fn(&[f32]) + Send + 'static,
    {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("IMMDeviceEnumerator")?;

            let render_device = if let Some(ref id) = loopback_device {
                if id.is_empty() || id == "default" {
                    enumerator.GetDefaultAudioEndpoint(eRender, eConsole).context("GetDefaultAudioEndpoint")?
                } else {
                    Self::find_render_device(&enumerator, id)
                        .unwrap_or_else(|_| enumerator.GetDefaultAudioEndpoint(eRender, eConsole).unwrap())
                }
            } else {
                enumerator.GetDefaultAudioEndpoint(eRender, eConsole).context("GetDefaultAudioEndpoint")?
            };

            let lb_client: IAudioClient = render_device.Activate(CLSCTX_ALL, None)
                .context("Activate loopback")?;
            let lb_fmt = lb_client.GetMixFormat().context("GetMixFormat")? as *const WAVEFORMATEX;
            lb_client.Initialize(
                AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK,
                2_000_000, 0, lb_fmt, None,
            ).context("Initialize loopback")?;
            let lb_capture: IAudioCaptureClient = lb_client.GetService()
                .context("GetService loopback")?;

            let fmt = parse_format(lb_fmt);
            let bpf = fmt.channels * (fmt.bps / 8);

            // Optional mic
            let (mic_client, mic_capture, mic_fmt, mic_bpf) = if let Some(ref id) = mic_device {
                if !id.is_empty() {
                    match Self::init_mic(&enumerator, id) {
                        Ok((c, cap, ptr)) => {
                            let mfmt = parse_format(ptr);
                            (Some(c), Some(cap), Some(mfmt), mfmt.channels * (mfmt.bps / 8))
                        }
                        Err(_) => (None, None, None, 0)
                    }
                } else { (None, None, None, 0) }
            } else { (None, None, None, 0) };

            let lb_event = CreateEventW(None, false, false, None).context("CreateEventW")?;
            lb_client.SetEventHandle(lb_event).context("SetEventHandle")?;
            lb_client.Start().context("Loopback Start")?;

            let mic_event = mic_client.as_ref().and_then(|mc| {
                let e = CreateEventW(None, false, false, None).ok()?;
                mc.SetEventHandle(e).ok()?;
                mc.Start().ok()?;
                Some(e)
            });

            let target_sr = fmt.sample_rate;
            let bps = fmt.bps / 8;

            // Simple capture loop on this thread — no spawning sub-threads
            // CRITICAL for A/V sync: we write audio at a CONSTANT RATE.
            // When no system audio is playing, we write silence (zeros).
            // This matches how Windows Game Bar handles audio — the audio
            // stream always advances at 48kHz regardless of activity.
            let mut total_audio_frames_written: u64 = 0;
            let loop_start = std::time::Instant::now();

            while !stop.load(Ordering::SeqCst) {
                let wait = WaitForSingleObject(lb_event, 10); // 10ms poll
                if stop.load(Ordering::SeqCst) { break; }

                if wait != WAIT_OBJECT_0 {
                    // No audio event — calculate how many frames SHOULD have been
                    // written by now based on elapsed wall-clock time, then write
                    // silence to fill the gap. This ensures audio always runs at
                    // exactly the sample rate regardless of timer resolution.
                    let elapsed_ms = loop_start.elapsed().as_millis() as u64;
                    let expected_frames = elapsed_ms * fmt.sample_rate as u64 / 1000;
                    if expected_frames > total_audio_frames_written {
                        let deficit = (expected_frames - total_audio_frames_written) as usize;
                        let silence = vec![0f32; deficit * 2]; // stereo
                        callback(&silence);
                        total_audio_frames_written += deficit as u64;
                    }
                    continue;
                }

                // Capture loopback audio
                loop {
                    let Ok(packet_frames) = lb_capture.GetNextPacketSize() else { break; };
                    if packet_frames == 0 { break; }
                    let mut ptr_raw = std::ptr::null_mut();
                    let mut frames = 0u32;
                    let mut flags = 0u32;
                    if lb_capture.GetBuffer(&mut ptr_raw, &mut frames, &mut flags, None, None).is_err() { break; }
                    if frames > 0 && !ptr_raw.is_null() {
                        let silent = (flags & 0x2) != 0;
                        let raw = std::slice::from_raw_parts(ptr_raw, frames as usize * bpf);
                        let mut samples = if silent {
                            vec![0f32; frames as usize * 2]
                        } else {
                            to_f32_stereo(raw, frames as usize, fmt.channels, bps, fmt.is_float)
                        };

                        // Mix mic if available
                        if let (Some(ref mc_cap), Some(mic_f)) = (&mic_capture, mic_fmt) {
                            let mic_bps = mic_f.bps / 8;
                            loop {
                                let Ok(mp) = mc_cap.GetNextPacketSize() else { break; };
                                if mp == 0 { break; }
                                let mut mp_raw = std::ptr::null_mut();
                                let mut mf = 0u32;
                                let mut mflags = 0u32;
                                if mc_cap.GetBuffer(&mut mp_raw, &mut mf, &mut mflags, None, None).is_err() { break; }
                                if mf > 0 && !mp_raw.is_null() {
                                    let msilent = (mflags & 0x2) != 0;
                                    let mraw = std::slice::from_raw_parts(mp_raw, mf as usize * mic_bpf);
                                    let mic_samples = if msilent {
                                        vec![0f32; mf as usize * 2]
                                    } else {
                                        to_f32_stereo(mraw, mf as usize, mic_f.channels, mic_bps, mic_f.is_float)
                                    };
                                    // Mix mic into loopback (simple sum, clamp)
                                    let mix_len = samples.len().min(mic_samples.len());
                                    for i in 0..mix_len {
                                        samples[i] = (samples[i] + mic_samples[i]).clamp(-1.0, 1.0);
                                    }
                                }
                                let _ = mc_cap.ReleaseBuffer(mf);
                            }
                        }

                        // Send audio chunk to callback
                        callback(&samples);
                        total_audio_frames_written += frames as u64;
                    }
                    let _ = lb_capture.ReleaseBuffer(frames);
                }
            }

            let _ = lb_client.Stop();
            use windows::Win32::Foundation::CloseHandle;
            CloseHandle(lb_event).ok();
            if let (Some(ref mc), Some(me)) = (&mic_client, mic_event) {
                let _ = mc.Stop();
                CloseHandle(me).ok();
            }
        }
        Ok(())
    }

    pub fn list_audio_devices() -> Result<Vec<serde_json::Value>> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)
                    .context("IMMDeviceEnumerator")?;
            let mut devices = Vec::new();

            // Enumerate render (output) endpoints
            let render_collection = enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE(1))
                .context("EnumAudioEndpoints render")?;
            let rcount = render_collection.GetCount()?;
            let device_name = |dev: &IMMDevice| -> String {
                if let Ok(store) = unsafe { dev.OpenPropertyStore(STGM_READ) } {
                    let key = windows::Win32::Foundation::PROPERTYKEY {
                        fmtid: windows_core::GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
                        pid: 14,
                    };
                    if let Ok(val) = unsafe { store.GetValue(&key) } {
                        let s = val.to_string();
                        if !s.is_empty() { return s; }
                    }
                }
                String::new()
            };

            for i in 0..rcount {
                if let Ok(dev) = render_collection.Item(i) {
                    if let Ok(id) = dev.GetId() {
                        if let Ok(id_str) = id.to_string() {
                            let name = device_name(&dev);
                            devices.push(serde_json::json!({"id": id_str, "name": name, "kind": "output"}));
                        }
                    }
                }
            }

            // Enumerate capture (input) endpoints
            let capture_collection = enumerator
                .EnumAudioEndpoints(eCapture, DEVICE_STATE(1))
                .context("EnumAudioEndpoints capture")?;
            let ccount = capture_collection.GetCount()?;
            for i in 0..ccount {
                if let Ok(dev) = capture_collection.Item(i) {
                    if let Ok(id) = dev.GetId() {
                        if let Ok(id_str) = id.to_string() {
                            let name = device_name(&dev);
                            devices.push(serde_json::json!({"id": id_str, "name": name, "kind": "input"}));
                        }
                    }
                }
            }

            Ok(devices)
        }
    }
}

unsafe fn loopback_capture_loop(
    _client: &IAudioClient,
    capture: &IAudioCaptureClient,
    event: windows::Win32::Foundation::HANDLE,
    stop: &AtomicBool,
    mixer: &Mutex<AudioMixer>,
    fmt: AudioFormat,
    bpf: usize,
) {
    let target_sr = fmt.sample_rate;
    let bps = fmt.bps / 8;
    let debug_path = std::env::temp_dir().join("clipsta_audio_debug.txt");
    let mut total_frames: u64 = 0;
    while !stop.load(Ordering::SeqCst) {
        let wait = WaitForSingleObject(event, 50);
        if stop.load(Ordering::SeqCst) { break; }
        if wait != WAIT_OBJECT_0 { continue; }
        loop {
            let Ok(packet_frames) = capture.GetNextPacketSize() else { break; };
            if packet_frames == 0 { break; }
            let mut ptr = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            if capture.GetBuffer(&mut ptr, &mut frames, &mut flags, None, None).is_err() { break; }
            if frames > 0 && !ptr.is_null() {
                let silent = (flags & 0x2) != 0;
                let raw = std::slice::from_raw_parts(ptr, frames as usize * bpf);
                let samples = if silent {
                    vec![0f32; frames as usize * 2]
                } else {
                    to_f32_stereo(raw, frames as usize, fmt.channels, bps, fmt.is_float)
                };
                if let Ok(mut m) = mixer.lock() {
                    m.loopback.extend(samples);
                    let max = target_sr as usize * 300 * 2;
                    if m.loopback.len() > max {
                        let excess = m.loopback.len() - max;
                        m.loopback.drain(0..excess);
                    }
                }
                total_frames += frames as u64;
                if total_frames % (target_sr as u64) == 0 {
                    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
                        use std::io::Write;
                        writeln!(f, "[{}] loopback: {} total frames captured", debug_timestamp(), total_frames)
                    });
                }
            }
            let _ = capture.ReleaseBuffer(frames);
        }
    }
    let _ = std::fs::OpenOptions::new().create(true).append(true).open(&debug_path).and_then(|mut f| {
        use std::io::Write;
        writeln!(f, "[{}] loopback capture thread exiting, total_frames={}", debug_timestamp(), total_frames)
    });
}

unsafe fn mic_capture_loop(
    _client: &IAudioClient,
    capture: &IAudioCaptureClient,
    event: windows::Win32::Foundation::HANDLE,
    stop: &AtomicBool,
    mixer: &Mutex<AudioMixer>,
    fmt: AudioFormat,
    bpf: usize,
    target_sr: u32,
) {
    let bps = fmt.bps / 8;
    let mut accum: Vec<f32> = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let wait = WaitForSingleObject(event, 50);
        if stop.load(Ordering::SeqCst) { break; }
        if wait != WAIT_OBJECT_0 { continue; }
        loop {
            let Ok(packet_frames) = capture.GetNextPacketSize() else { break; };
            if packet_frames == 0 { break; }
            let mut ptr = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            if capture.GetBuffer(&mut ptr, &mut frames, &mut flags, None, None).is_err() { break; }
            if frames > 0 && !ptr.is_null() {
                let silent = (flags & 0x2) != 0;
                let raw = std::slice::from_raw_parts(ptr, frames as usize * bpf);
                let mut samples = if silent {
                    vec![0f32; frames as usize * 2]
                } else {
                    to_f32_stereo(raw, frames as usize, fmt.channels, bps, fmt.is_float)
                };
                if fmt.sample_rate != target_sr {
                    accum.extend(samples.drain(..));
                    let ratio = target_sr as f64 / fmt.sample_rate as f64;
                    let out_count = (accum.len() as f64 * ratio / 2.0) as usize * 2;
                    if out_count > 0 {
                        samples = linear_resample(&accum, out_count);
                        accum.clear();
                    } else {
                        samples.clear();
                    }
                }
                if !samples.is_empty() {
                    if let Ok(mut m) = mixer.lock() {
                        m.mic.extend(samples);
                        let max = target_sr as usize * 300 * 2;
                        if m.mic.len() > max {
                            let excess = m.mic.len() - max;
                            m.mic.drain(0..excess);
                        }
                    }
                }
            }
            let _ = capture.ReleaseBuffer(frames);
        }
    }
}

fn linear_resample(input: &[f32], out_len: usize) -> Vec<f32> {
    if input.len() < 4 || out_len < 4 {
        return vec![0.0; out_len.max(2)];
    }
    let in_frames = input.len() / 2;
    let out_frames = out_len / 2;
    let ratio = in_frames as f64 / out_frames as f64;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_frames {
        let pos = i as f64 * ratio;
        let idx = (pos.floor() as usize).min(in_frames - 1);
        let frac = (pos - pos.floor()) as f32;
        let next = (idx + 1).min(in_frames - 1);
        out.push(input[idx * 2] * (1.0 - frac) + input[next * 2] * frac);
        out.push(input[idx * 2 + 1] * (1.0 - frac) + input[next * 2 + 1] * frac);
    }
    out
}

// ── PCM conversion helpers ────────────────────────────────────────────────────

fn to_f32_stereo(data: &[u8], frames: usize, ch: usize, bps: usize, float: bool) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * 2);
    let bpf = ch * bps;
    for f in 0..frames {
        let b = f * bpf;
        if b + bps > data.len() { out.push(0.0); out.push(0.0); continue; }
        let l = read_sample(&data[b..], bps, float);
        let r = if ch >= 2 && b + bps * 2 <= data.len() {
            read_sample(&data[b + bps..], bps, float)
        } else { l };
        out.push(l);
        out.push(r);
    }
    out
}

fn read_sample(d: &[u8], bps: usize, float: bool) -> f32 {
    if d.len() < bps { return 0.0; }
    if float {
        match bps {
            4 => f32::from_le_bytes([d[0], d[1], d[2], d[3]]),
            8 => f64::from_le_bytes([d[0],d[1],d[2],d[3],d[4],d[5],d[6],d[7]]) as f32,
            _ => 0.0,
        }
    } else {
        match bps {
            2 => i16::from_le_bytes([d[0], d[1]]) as f32 / 32768.0,
            3 => {
                let v = (d[0] as i32) | ((d[1] as i32) << 8) | ((d[2] as i32) << 16);
                let v = if v & 0x800000 != 0 { v | !0xFFFFFF } else { v };
                v as f32 / 8388608.0
            }
            4 => i32::from_le_bytes([d[0],d[1],d[2],d[3]]) as f32 / 2147483648.0,
            _ => 0.0,
        }
    }
}
