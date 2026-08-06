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
		if (!wgcActiveRef.current) {
			setState((s) => ({
				...s,
				error: "Recording starting — try again in a moment.",
			}));
			return null;
		}
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
				error: savedPath ? null : "Recording — clip will be available in a few seconds.",
			}));
			// Auto-dismiss the "available in a few seconds" message
			if (!savedPath) {
				setTimeout(() => setState((s) => ({ ...s, error: s.error?.includes("available") ? null : s.error })), 3000);
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

	// Auto-start capture
	const startCaptureRef = useRef(startCapture);
	startCaptureRef.current = startCapture;

	useEffect(() => {
		if (!settings) return;
		if (wgcActiveRef.current) return;

		const timer = setTimeout(() => {
			if (!wgcActiveRef.current) {
				startCaptureRef.current();
			}
		}, 0);
		return () => clearTimeout(timer);
	}, [settings?.fps]);

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

				// === DSLR Camera Shutter Sound ===
				// 1. Mirror slap (sharp attack, low-mid frequency thud)
				const mirrorLen = Math.floor(ctx.sampleRate * 0.015);
				const mirrorBuf = ctx.createBuffer(1, mirrorLen, ctx.sampleRate);
				const mirrorData = mirrorBuf.getChannelData(0);
				for (let i = 0; i < mirrorLen; i++) {
					const t = i / ctx.sampleRate;
					mirrorData[i] = Math.sin(t * 800 * Math.PI * 2) * Math.exp(-i / (mirrorLen * 0.2)) * 0.8
						+ (Math.random() * 2 - 1) * Math.exp(-i / (mirrorLen * 0.1)) * 0.3;
				}
				const mirror = ctx.createBufferSource();
				mirror.buffer = mirrorBuf;
				const mirrorGain = ctx.createGain();
				mirrorGain.gain.setValueAtTime(0.7, now);
				mirrorGain.gain.exponentialRampToValueAtTime(0.001, now + 0.02);
				mirror.connect(mirrorGain);
				mirrorGain.connect(ctx.destination);
				mirror.start(now);

				// 2. Shutter curtain (quick mechanical slide — band-passed noise)
				const curtainLen = Math.floor(ctx.sampleRate * 0.04);
				const curtainBuf = ctx.createBuffer(1, curtainLen, ctx.sampleRate);
				const curtainData = curtainBuf.getChannelData(0);
				for (let i = 0; i < curtainLen; i++) {
					curtainData[i] = (Math.random() * 2 - 1) * Math.exp(-i / (curtainLen * 0.25));
				}
				const curtain = ctx.createBufferSource();
				curtain.buffer = curtainBuf;
				const bp = ctx.createBiquadFilter();
				bp.type = "bandpass";
				bp.frequency.value = 3500;
				bp.Q.value = 1.5;
				const curtainGain = ctx.createGain();
				curtainGain.gain.setValueAtTime(0.5, now + 0.015);
				curtainGain.gain.exponentialRampToValueAtTime(0.001, now + 0.06);
				curtain.connect(bp);
				bp.connect(curtainGain);
				curtainGain.connect(ctx.destination);
				curtain.start(now + 0.015);

				// 3. Body resonance (subtle low thump that gives weight)
				const bodyOsc = ctx.createOscillator();
				bodyOsc.type = "sine";
				bodyOsc.frequency.setValueAtTime(180, now);
				bodyOsc.frequency.exponentialRampToValueAtTime(80, now + 0.05);
				const bodyGain = ctx.createGain();
				bodyGain.gain.setValueAtTime(0.3, now);
				bodyGain.gain.exponentialRampToValueAtTime(0.001, now + 0.06);
				bodyOsc.connect(bodyGain);
				bodyGain.connect(ctx.destination);
				bodyOsc.start(now);
				bodyOsc.stop(now + 0.07);

				// 4. Second curtain (closing) — slightly delayed, softer
				const curtain2Len = Math.floor(ctx.sampleRate * 0.025);
				const curtain2Buf = ctx.createBuffer(1, curtain2Len, ctx.sampleRate);
				const curtain2Data = curtain2Buf.getChannelData(0);
				for (let i = 0; i < curtain2Len; i++) {
					curtain2Data[i] = (Math.random() * 2 - 1) * Math.exp(-i / (curtain2Len * 0.2));
				}
				const curtain2 = ctx.createBufferSource();
				curtain2.buffer = curtain2Buf;
				const bp2 = ctx.createBiquadFilter();
				bp2.type = "bandpass";
				bp2.frequency.value = 2800;
				bp2.Q.value = 2;
				const curtain2Gain = ctx.createGain();
				curtain2Gain.gain.setValueAtTime(0.35, now + 0.055);
				curtain2Gain.gain.exponentialRampToValueAtTime(0.001, now + 0.09);
				curtain2.connect(bp2);
				bp2.connect(curtain2Gain);
				curtain2Gain.connect(ctx.destination);
				curtain2.start(now + 0.055);
			} catch { /* ignore */ }
		});
		return () => {
			unlistenPromise.then((u) => u()).catch(() => {});
			// Close the pooled AudioContext on unmount
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
