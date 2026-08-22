import { useCallback, useEffect, useRef, useState } from "react";
import type { AppSettings } from "../types";
import bridge from "../tauri-bridge";

export type RecordState = "idle" | "recording" | "saving";

export interface RecorderState {
	status: RecordState;
	duration: number;
	savedPath: string | null;
	error: string | null;
	bufferSeconds: number;
	captureWidth: number;
	captureHeight: number;
	captureFps: number;
}

function pad(n: number) { return String(n).padStart(2, "0"); }

/**
 * ShadowPlay-style file naming: "{GameName} {YYYY.MM.DD} - {HH.MM.SS.ff}.DVR.mp4"
 * Example: "Battlefield 6 2026.07.26 - 19.56.14.04.DVR.mp4"
 */
function makeFileName(_label: string, ext: string, gameName?: string): string {
	const now = new Date();
	const date = `${now.getFullYear()}.${pad(now.getMonth() + 1)}.${pad(now.getDate())}`;
	const time = `${pad(now.getHours())}.${pad(now.getMinutes())}.${pad(now.getSeconds())}.${pad(Math.floor(now.getMilliseconds() / 10))}`;
	const name = gameName || (window as any).__clipsta_active_game || "Desktop";
	return `${name} ${date} - ${time}.DVR.${ext}`;
}

export function useRecorder(settings: AppSettings | null) {
	const [state, setState] = useState<RecorderState>({
		status: "idle",
		duration: 0,
		savedPath: null,
		error: null,
		bufferSeconds: 0,
		captureWidth: 0,
		captureHeight: 0,
		captureFps: 0,
	});

	const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
	const durationRef = useRef(0);
	const wgcActiveRef = useRef(false);

	function startTimer() {
		durationRef.current = 0;
		if (timerRef.current) { clearInterval(timerRef.current); timerRef.current = null; }
		timerRef.current = setInterval(() => {
			durationRef.current += 1;
			setState((s) => ({ ...s, duration: durationRef.current }));
		}, 1000);
	}

	function stopTimer() {
		if (timerRef.current) { clearInterval(timerRef.current); timerRef.current = null; }
		durationRef.current = 0;
	}

	const retryCountRef = useRef(0);
	const startCapture = useCallback(async () => {
		if (wgcActiveRef.current) return;
		try {
			setState((s) => ({ ...s, error: null, status: "recording" }));

			const noAudio = settings?.audioSource === "none" || !(settings?.captureAudio ?? true);
			const micDevice = (settings?.audioSource === "mic" || settings?.audioSource === "both")
				? (settings?.audioInputDeviceId || "default")
				: undefined;
			const loopbackDevice = (settings?.audioSource === "desktop" || settings?.audioSource === "both")
				? (settings?.desktopAudioDeviceId || "default")
				: undefined;

			const result = await bridge.wgcStartRecording({
				sourceId: null,
				fps: settings?.fps ?? 60,
				noAudio,
				micDevice,
				loopbackDevice,
			});

			if (!result) {
				setState((s) => ({ ...s, status: "idle", error: "Capture failed to start" }));
				retryCountRef.current++;
				if (retryCountRef.current < 3) {
					setTimeout(() => { if (!wgcActiveRef.current) startCapture(); }, 2000);
				} else {
					setState((s) => ({ ...s, error: "Capture failed after 3 attempts. Check GPU drivers." }));
				}
				return;
			}

			retryCountRef.current = 0;
			wgcActiveRef.current = true;
			await bridge.setRecordingState(true);
			setState((s) => ({
				...s,
				captureWidth: result.width ?? 0,
				captureHeight: result.height ?? 0,
				captureFps: result.fps ?? 0,
			}));
			startTimer();
		} catch (err: any) {
			setState((s) => ({ ...s, status: "idle", error: err.message ?? "Capture failed" }));
			retryCountRef.current++;
			if (retryCountRef.current < 3) {
				setTimeout(() => { if (!wgcActiveRef.current) startCapture(); }, 2000);
			} else {
				setState((s) => ({ ...s, error: "Capture failed after 3 attempts. Check GPU drivers." }));
			}
		}
	}, [settings]);

	const savingRef = useRef(false);
	const saveClip = useCallback(async (seconds: number): Promise<string | null> => {
		// Don't gate on wgcActiveRef — the backend's is_recording flag is the
		// source of truth. If WebView2 recovered from a crash, the frontend state
		// may be stale but the capture thread is still running in the background.
		// Prevent duplicate saves (hotkey repeat, double-click, etc)
		if (savingRef.current) return null;
		savingRef.current = true;
		try {
			setState((s) => ({ ...s, status: "saving", error: null }));
			const label = seconds <= 30 ? "30sec" : seconds <= 60 ? "1min" : "5min";
			// Query active window title now (at save time) instead of constant polling
			let activeGame = (window as any).__clipsta_active_game || "Desktop";
			try {
				activeGame = await bridge.getActiveWindowTitle();
				(window as any).__clipsta_active_game = activeGame;
			} catch { /* keep last known */ }
			const fileName = makeFileName(label, "mp4", activeGame);
			const noAudio = settings?.audioSource === "none" || !(settings?.captureAudio ?? true);
			const micDevice = (settings?.audioSource === "mic" || settings?.audioSource === "both")
				? (settings?.audioInputDeviceId || "default")
				: undefined;
			const loopbackDevice = (settings?.audioSource === "desktop" || settings?.audioSource === "both")
				? (settings?.desktopAudioDeviceId || "default")
				: undefined;

			const savedPath = await bridge.wgcSaveClip({
				seconds,
				fileName,
				sourceId: null,
				fps: settings?.fps ?? 60,
				noAudio,
				micDevice,
				loopbackDevice,
			});

			setState((s) => ({
				...s,
				status: "recording",
				savedPath,
				error: savedPath ? null : "Buffering — wait a few seconds for the first keyframe.",
			}));
			// Auto-dismiss the message
			if (!savedPath) {
				setTimeout(() => setState((s) => ({ ...s, error: s.error?.includes("Buffering") ? null : s.error })), 3000);
			}
			return savedPath;
		} catch (err: any) {
			setState((s) => ({
				...s,
				status: "recording",
				savedPath: null,
				error: err?.message ?? "Clip save failed",
			}));
			// Auto-dismiss error after 5 seconds
			setTimeout(() => setState((s) => ({ ...s, error: null })), 5000);
			return null;
		} finally {
			savingRef.current = false;
		}
	}, [settings]);

	// Hotkeys via Tauri events
	const saveRef = useRef(saveClip);
	saveRef.current = saveClip;

	useEffect(() => {
		const unlisteners: Promise<() => void>[] = [];

		unlisteners.push(bridge.onHotkeyRecord(() => {}));
		unlisteners.push(bridge.onHotkeyClip1Min(() => saveRef.current(60)));
		unlisteners.push(bridge.onHotkeyClip5Min(() => saveRef.current(300)));
		unlisteners.push(bridge.onHotkeyClip30Sec(() => saveRef.current(30)));

		return () => {
			unlisteners.forEach((p) => p.then((unlisten) => unlisten()).catch(() => {}));
		};
	}, []);

	// Auto-start capture, and restart when resolution/fps changes
	const startCaptureRef = useRef(startCapture);
	startCaptureRef.current = startCapture;

	useEffect(() => {
		if (!settings) return;

		// If capture is already running and a key setting changed, restart it
		if (wgcActiveRef.current) {
			// Stop current capture, then restart with new settings
			wgcActiveRef.current = false;
			stopTimer();
			bridge.wgcStopRecording().then(() => {
				bridge.setRecordingState(false).then(() => {
					setTimeout(() => {
						if (!wgcActiveRef.current) {
							startCaptureRef.current();
						}
					}, 500); // Brief delay for GPU resources to release
				});
			}).catch(() => {
				setTimeout(() => {
					if (!wgcActiveRef.current) {
						startCaptureRef.current();
					}
				}, 1000);
			});
			return;
		}

		const timer = setTimeout(() => {
			if (!wgcActiveRef.current) {
				startCaptureRef.current();
			}
		}, 0);
		return () => clearTimeout(timer);
	}, [settings?.fps, settings?.resolution]);

	// ShadowPlay-style game detection: query active window only at save time.
	// No constant polling — getActiveWindowTitle() is called in makeFileName().
	// We just seed the initial value on mount.
	useEffect(() => {
		bridge.getActiveWindowTitle().then((title) => {
			(window as any).__clipsta_active_game = title;
		}).catch(() => {});
	}, []);

	// WGC clip-saved event
	useEffect(() => {
		const unlistenPromise = bridge.onWgcClipSaved((savedPath: string) => {
			setState((s) => ({ ...s, savedPath }));
		});
		return () => {
			if (unlistenPromise && typeof unlistenPromise === "object" && "then" in unlistenPromise) {
				(unlistenPromise as Promise<() => void>).then((u) => u()).catch(() => {});
			}
		};
	}, []);

	// Auto-recovery: if capture dies unexpectedly (GPU reset, mode switch, alt-tab in
	// fullscreen exclusive), automatically restart. Caps at 3 restarts per mount to
	// avoid crash loops. User sees brief "Restarting..." then recording resumes.
	const autoRestartCountRef = useRef(0);
	useEffect(() => {
		let cancelled = false;
		const unlistenPromise = import("@tauri-apps/api/event").then(({ listen }) =>
			listen<string>("wgc:capture-lost", (event) => {
				if (cancelled) return;
				if (autoRestartCountRef.current >= 3) {
					setState((s) => ({ ...s, status: "idle", error: "Recording stopped: " + (event.payload || "capture lost") }));
					wgcActiveRef.current = false;
					stopTimer();
					return;
				}
				autoRestartCountRef.current++;
				wgcActiveRef.current = false;
				// Brief pause then restart
				setTimeout(() => {
					if (!cancelled && !wgcActiveRef.current) {
						startCaptureRef.current();
					}
				}, 1500);
			})
		);
		return () => {
			cancelled = true;
			unlistenPromise.then((u) => u()).catch(() => {});
		};
	}, []);

	// Clip sound — realistic DSLR camera shutter (pooled AudioContext for performance)
	const audioCtxRef = useRef<AudioContext | null>(null);
	useEffect(() => {
		const unlistenPromise = bridge.onPlayClipSound(() => {
			try {
				if (!audioCtxRef.current || audioCtxRef.current.state === "closed") {
					audioCtxRef.current = new AudioContext();
				}
				const ctx = audioCtxRef.current;
				if (ctx.state === "suspended") {
					ctx.resume().catch(() => {});
				}
				const now = ctx.currentTime;

				// === Clean "Clip Saved" notification tone ===
				// Two-tone ascending ding (similar to ShadowPlay/Medal save sound)
				// Short, satisfying, non-intrusive

				// Tone 1: lower note (E5 = 659 Hz)
				const osc1 = ctx.createOscillator();
				osc1.type = "sine";
				osc1.frequency.value = 659;
				const gain1 = ctx.createGain();
				gain1.gain.setValueAtTime(0.4, now);
				gain1.gain.exponentialRampToValueAtTime(0.001, now + 0.15);
				osc1.connect(gain1);
				gain1.connect(ctx.destination);
				osc1.start(now);
				osc1.stop(now + 0.15);

				// Tone 2: higher note (B5 = 988 Hz), slightly delayed
				const osc2 = ctx.createOscillator();
				osc2.type = "sine";
				osc2.frequency.value = 988;
				const gain2 = ctx.createGain();
				gain2.gain.setValueAtTime(0.35, now + 0.08);
				gain2.gain.exponentialRampToValueAtTime(0.001, now + 0.25);
				osc2.connect(gain2);
				gain2.connect(ctx.destination);
				osc2.start(now + 0.08);
				osc2.stop(now + 0.25);
			} catch { /* ignore */ }
		});
		return () => {
			unlistenPromise.then((u) => u()).catch(() => {});
			audioCtxRef.current?.close().catch(() => {});
			audioCtxRef.current = null;
		};
	}, []);

	// Cleanup on unmount
	useEffect(() => {
		return () => {
			stopTimer();
			if (wgcActiveRef.current) {
				wgcActiveRef.current = false;
				bridge.wgcStopRecording().catch(() => {});
				bridge.setRecordingState(false).catch(() => {});
			}
		};
	}, []);

	return {
		state,
		saveClip,
		isActive: wgcActiveRef.current || state.status === "recording",
	};
}
