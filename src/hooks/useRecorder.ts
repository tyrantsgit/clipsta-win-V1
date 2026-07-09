import { useCallback, useEffect, useRef, useState } from "react";
import type { AppSettings } from "../types";

export type RecordState = "idle" | "recording" | "saving";

export interface RecorderState {
	status: RecordState;
	duration: number;
	savedPath: string | null;
	error: string | null;
	bufferSeconds: number;
}

function pad(n: number) { return String(n).padStart(2, "0"); }

function makeFileName(label: string, ext: string): string {
	const now = new Date();
	const stamp = `${now.getFullYear()}${pad(now.getMonth()+1)}${pad(now.getDate())}_${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
	return `Clipsta_${label}_${stamp}.${ext}`;
}

/**
 * useRecorder – WGC-first recording hook.
 * 
 * When window.clipsta.wgcStartRecording is available (Electron + WGC binary),
 * recording is delegated entirely to the native clipsta-capture process via IPC.
 * 
 * Falls back to MediaRecorder/getDisplayMedia for non-WGC contexts (web, dev).
 */
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
	const sourceIdRef = useRef<string | null>(null);

	// ── WGC mode refs ──────────────────────────────────────────────────────
	const wgcActiveRef = useRef(false);
	const userStoppedRef = useRef(false);

	// ── Legacy MediaRecorder mode refs ─────────────────────────────────────
	const recorderRef = useRef<MediaRecorder | null>(null);
	const bufferRef = useRef<{ chunk: Blob; ts: number }[]>([]);
	const streamRef = useRef<MediaStream | null>(null);
	const auxStreamsRef = useRef<MediaStream[]>([]);

	const isWgcAvailable = typeof window !== "undefined" && typeof window.clipsta?.wgcStartRecording === "function";

	// ── Duration timer ─────────────────────────────────────────────────────
	function startTimer() {
		durationRef.current = 0;
		clearInterval(timerRef.current!);
		timerRef.current = setInterval(() => {
			durationRef.current++;
			setState((s) => ({ ...s, duration: durationRef.current }));
		}, 1000);
	}

	function stopTimer() {
		clearInterval(timerRef.current!);
		durationRef.current = 0;
	}

	// ── WGC: start recording ───────────────────────────────────────────────
	const startWgcRecording = useCallback(async (sourceId: string | null) => {
		try {
			userStoppedRef.current = false; // Allow auto-restart again
			setState((s) => ({ ...s, error: null, status: "recording" }));
			
			const noAudio = settings?.audioSource === "none" || !(settings?.captureAudio ?? true);
			const micDevice = (settings?.audioSource === "mic" || settings?.audioSource === "both")
				? (settings?.audioInputDeviceId || "default")
				: undefined;
			const loopbackDevice = (settings?.audioSource === "desktop" || settings?.audioSource === "both")
				? (settings?.desktopAudioDeviceId || "default")
				: undefined;

			const result = await window.clipsta!.wgcStartRecording({
				sourceId,
				fps: settings?.fps ?? 60,
				noAudio,
				micDevice,
				loopbackDevice,
			});

			if (!result) {
				setState((s) => ({ ...s, status: "idle", error: "WGC capture failed to start" }));
				return;
			}

			wgcActiveRef.current = true;
			await window.clipsta!.setRecordingState(true);
			startTimer();
		} catch (err: any) {
			setState((s) => ({ ...s, status: "idle", error: err.message ?? "WGC capture failed" }));
		}
	}, [settings]);

	// ── WGC: stop recording ─────────────────────────────────────────────────
	const stopWgcRecording = useCallback(async () => {
		if (!wgcActiveRef.current) return;
		wgcActiveRef.current = false;
		userStoppedRef.current = true; // Prevent auto-restart
		stopTimer();
		await window.clipsta!.wgcStopRecording();
		await window.clipsta!.setRecordingState(false);
		setState((s) => ({ ...s, status: "idle", duration: 0 }));
	}, []);

	// ── WGC: save clip (extracts last N seconds from live recording buffer) ──
	const saveWgcClip = useCallback(async (seconds: number): Promise<string | null> => {
		// If recording isn't active yet, we can't extract a clip
		if (!wgcActiveRef.current) {
			setState((s) => ({
				...s,
				error: "Buffering... recording is starting up. Try again in a few seconds.",
			}));
			// Trigger auto-start if not already started
			if (state.status === "idle") {
				startWgcRecording(sourceIdRef.current);
			}
			return null;
		}
		try {
			setState((s) => ({ ...s, status: "saving" }));
			const label = seconds <= 60 ? "1min" : "5min";
			const fileName = makeFileName(label, "mp4");
			const noAudio = settings?.audioSource === "none" || !(settings?.captureAudio ?? true);
			const micDevice = (settings?.audioSource === "mic" || settings?.audioSource === "both")
				? (settings?.audioInputDeviceId || "default")
				: undefined;
			const loopbackDevice = (settings?.audioSource === "desktop" || settings?.audioSource === "both")
				? (settings?.desktopAudioDeviceId || "default")
				: undefined;
			const savedPath = await window.clipsta!.wgcSaveClip({
				seconds,
				fileName,
				sourceId: sourceIdRef.current,
				fps: settings?.fps ?? 60,
				noAudio,
				micDevice,
				loopbackDevice,
			});

			// The backend stops recording to extract the clip, then signals restart.
			// Mark as not active so the auto-start effect can restart.
			wgcActiveRef.current = false;
			setState((s) => ({
				...s,
				status: "idle",
				savedPath,
				error: savedPath ? null : "Clip save failed — not enough recording time yet.",
			}));

			// Auto-restart recording after a brief delay
			setTimeout(() => {
				if (!wgcActiveRef.current) {
					startWgcRecording(sourceIdRef.current);
				}
			}, 1000);

			return savedPath;
		} catch (err: any) {
			wgcActiveRef.current = false;
			setState((s) => ({
				...s,
				status: "idle",
				savedPath: null,
				error: err?.message ?? "Clip save threw an error",
			}));
			// Restart recording
			setTimeout(() => { startWgcRecording(sourceIdRef.current); }, 1000);
			return null;
		}
	}, [settings, state.status, startWgcRecording]);

	// ── Legacy MediaRecorder: mix audio tracks ─────────────────────────────
	async function ensureSingleAudioTrack(stream: MediaStream): Promise<MediaStream> {
		const tracks = stream.getAudioTracks();
		if (tracks.length <= 1) return stream;
		try {
			const ctx = new AudioContext();
			const dest = ctx.createMediaStreamDestination();
			for (const track of tracks) {
				ctx.createMediaStreamSource(new MediaStream([track])).connect(dest);
			}
			tracks.forEach((t) => { stream.removeTrack(t); t.stop(); });
			dest.stream.getAudioTracks().forEach((t) => stream.addTrack(t));
			await ctx.close();
		} catch {
			const keep = tracks[0];
			for (let i = 1; i < tracks.length; i++) { stream.removeTrack(tracks[i]); tracks[i].stop(); }
		}
		return stream;
	}

	// ── Legacy: get display stream ─────────────────────────────────────────
	const getStream = useCallback(async (sourceId: string | null): Promise<MediaStream> => {
		const audioSource = settings?.audioSource ?? "desktop";
		await window.clipsta?.setPendingSource(sourceId);
		const stream = await navigator.mediaDevices.getDisplayMedia({
			video: { frameRate: settings?.fps ?? 60 },
			audio: true,
		});
		if (audioSource !== "desktop" && audioSource !== "both") {
			stream.getAudioTracks().forEach((t) => { stream.removeTrack(t); t.stop(); });
		}
		const auxStreams: MediaStream[] = [];
		if (audioSource === "mic" || audioSource === "both") {
			try {
				const constraints: MediaTrackConstraints = { echoCancellation: true, noiseSuppression: true };
				if (settings?.audioInputDeviceId) constraints.deviceId = { exact: settings.audioInputDeviceId };
				const mic = await navigator.mediaDevices.getUserMedia({ audio: constraints });
				auxStreams.push(mic);
				mic.getAudioTracks().forEach((t: MediaStreamTrack) => stream.addTrack(t));
			} catch { /* mic unavailable */ }
		}
		auxStreamsRef.current = auxStreams;
		await ensureSingleAudioTrack(stream);
		return stream;
	}, [settings]);

	// ── Legacy: start MediaRecorder ────────────────────────────────────────
	const CHUNKS_PER_SECOND = 10;
	const startLegacyRecording = useCallback(async (sourceId: string | null) => {
		try {
			setState((s) => ({ ...s, error: null, status: "recording" }));
			const stream = await getStream(sourceId);
			streamRef.current = stream;
			const capturedAux = auxStreamsRef.current;
			bufferRef.current = [];

			const mimeType = ["video/webm;codecs=vp9,opus","video/webm;codecs=vp8,opus","video/webm"]
				.find((m) => MediaRecorder.isTypeSupported(m)) ?? "video/webm";
			const recorder = new MediaRecorder(stream, {
				mimeType,
				videoBitsPerSecond: (settings?.bitrate ?? 8000) * 1000,
			});
			recorderRef.current = recorder;

			recorder.ondataavailable = (e) => {
				if (e.data.size > 0) {
					bufferRef.current.push({ chunk: e.data, ts: Date.now() });
					const keepMs = ((settings?.bufferDuration ?? 60) + 10) * 1000;
					const cutoff = Date.now() - keepMs;
					bufferRef.current = bufferRef.current.filter((c) => c.ts >= cutoff);
				}
			};

			recorder.onstop = () => {
				stopTimer();
				stream.getTracks().forEach((t) => t.stop());
				capturedAux.forEach((s) => s.getTracks().forEach((t) => t.stop()));
				auxStreamsRef.current = [];
			};

			recorder.start(1000 / CHUNKS_PER_SECOND);
			startTimer();
			window.clipsta?.setRecordingState(true);
		} catch (err: any) {
			setState((s) => ({ ...s, status: "idle", error: err.message ?? "Capture failed" }));
		}
	}, [getStream, settings]);

	// ── Legacy: stop MediaRecorder ─────────────────────────────────────────
	const stopLegacyRecording = useCallback(() => {
		recorderRef.current?.stop();
		stopTimer();
		setState((s) => ({ ...s, status: "idle", duration: 0 }));
		window.clipsta?.setRecordingState(false);
	}, []);

	// ── Legacy: save clip from buffer ──────────────────────────────────────
	const saveLegacyClip = useCallback(async (seconds: number): Promise<string | null> => {
		if (!bufferRef.current.length) return null;
		const wasRecording = state.status === "recording";
		setState((s) => ({ ...s, status: "saving" }));

		const cutoff = Date.now() - seconds * 1000;
		const chunks = bufferRef.current.filter((c) => c.ts >= cutoff).map((c) => c.chunk);

		if (!chunks.length) {
			setState((s) => ({ ...s, status: wasRecording ? "recording" : s.status, error: "Not enough buffer yet" }));
			return null;
		}

		const mimeType = recorderRef.current?.mimeType ?? "video/webm";
		const blob = new Blob(chunks, { type: mimeType });
		const arrayBuffer = await blob.arrayBuffer();
		const label = seconds <= 60 ? "1min" : "5min";
		const fileName = makeFileName(label, "webm");

		let savedPath: string | null = null;
		if (window.clipsta) {
			savedPath = await window.clipsta.saveRecording(arrayBuffer, fileName);
		} else {
			const url = URL.createObjectURL(blob);
			const a = document.createElement("a");
			a.href = url; a.download = fileName; a.click();
			URL.revokeObjectURL(url);
		}
		setState((s) => ({ ...s, status: wasRecording ? "recording" : s.status, savedPath }));
		return savedPath;
	}, [state.status]);

	// ── Public API: dispatch to WGC or legacy ─────────────────────────────

	const startRecording = useCallback(async (sourceId: string | null = null) => {
		if (isWgcAvailable) {
			await startWgcRecording(sourceId);
		} else {
			await startLegacyRecording(sourceId);
		}
	}, [isWgcAvailable, startWgcRecording, startLegacyRecording]);

	const stopRecording = useCallback(() => {
		if (isWgcAvailable) {
			stopWgcRecording();
		} else {
			stopLegacyRecording();
		}
	}, [isWgcAvailable, stopWgcRecording, stopLegacyRecording]);

	const saveClip = useCallback(async (seconds: number): Promise<string | null> => {
		if (isWgcAvailable) {
			return saveWgcClip(seconds);
		} else {
			return saveLegacyClip(seconds);
		}
	}, [isWgcAvailable, saveWgcClip, saveLegacyClip]);

	const saveFullRecording = useCallback(async (): Promise<string | null> => {
		if (isWgcAvailable && wgcActiveRef.current) {
			wgcActiveRef.current = false;
			stopTimer();
			await window.clipsta!.setRecordingState(false);
			setState((s) => ({ ...s, status: "idle", duration: 0 }));
			return window.clipsta!.wgcSaveFullRecording();
		} else {
			const result = await saveClip(durationRef.current);
			stopRecording();
			return result;
		}
	}, [isWgcAvailable, saveClip, stopRecording]);

	const toggleRecording = useCallback(
		async (sourceId: string | null = null) => {
			if (state.status === "recording") stopRecording();
			else await startRecording(sourceId);
		},
		[state.status, startRecording, stopRecording]
	);

	// ── Hotkeys ─────────────────────────────────────────────────────────────
	const toggleRef = useRef(toggleRecording);
	const saveRef = useRef(saveClip);
	toggleRef.current = toggleRecording;
	saveRef.current = saveClip;

	useEffect(() => {
		if (!window.clipsta) return;
		// Each registration replaces the previous listener in the preload
		window.clipsta.onHotkeyRecord(() => toggleRef.current(sourceIdRef.current));
		window.clipsta.onHotkeyClip1Min(() => saveRef.current(60));
		window.clipsta.onHotkeyClip5Min(() => saveRef.current(300));
		// Cleanup: register no-op replacements so stale closures can't fire
		return () => {
			window.clipsta?.onHotkeyRecord(() => {});
			window.clipsta?.onHotkeyClip1Min(() => {});
			window.clipsta?.onHotkeyClip5Min(() => {});
		};
	}, []);

	// ── Auto-start recording when WGC is available ────────────────────────
	// The recording runs continuously in the background (like ShadowPlay/Game Bar).
	// "Save 1 Min" extracts from the live buffer without needing manual start.
	// If user manually stops the buffer, don't auto-restart.
	useEffect(() => {
		if (!isWgcAvailable || !settings) return;
		if (wgcActiveRef.current) return;
		if (state.status === "saving" || state.status === "recording") return;
		if (userStoppedRef.current) return; // User explicitly stopped — don't restart

		const timer = setTimeout(() => {
			if (!wgcActiveRef.current && state.status === "idle" && !userStoppedRef.current) {
				startRecording(sourceIdRef.current);
			}
		}, 2000);
		return () => clearTimeout(timer);
	}, [isWgcAvailable, settings, state.status, startRecording]);

	// ── WGC clip-saved event ──────────────────────────────────────────────
	useEffect(() => {
		if (!window.clipsta?.onWgcClipSaved) return;
		window.clipsta.onWgcClipSaved((savedPath: string) => {
			setState((s) => ({ ...s, savedPath }));
		});
		return () => {
			window.clipsta?.onWgcClipSaved(() => {});
		};
	}, []);

	return {
		state,
		startRecording,
		stopRecording,
		toggleRecording,
		saveClip,
		saveFullRecording,
		setSourceId: (id: string | null) => { sourceIdRef.current = id; },
	};
}
