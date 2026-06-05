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

const CHUNKS_PER_SECOND = 1;

export function useRecorder(settings: AppSettings | null) {
	const [state, setState] = useState<RecorderState>({
		status: "idle",
		duration: 0,
		savedPath: null,
		error: null,
		bufferSeconds: 0,
	});

	const recorderRef = useRef<MediaRecorder | null>(null);
	const bufferRef = useRef<{ chunk: Blob; ts: number }[]>([]);
	const streamRef = useRef<MediaStream | null>(null);
	const auxStreamsRef = useRef<MediaStream[]>([]);
	const timerRef = useRef<ReturnType<typeof setInterval> | null>(null);
	const durationRef = useRef(0);
	const sourceIdRef = useRef<string | null>(null);

	// ── Mix multiple audio tracks into one ─────────────────────────────────
	async function ensureSingleAudioTrack(stream: MediaStream): Promise<MediaStream> {
		const tracks = stream.getAudioTracks();
		if (tracks.length <= 1) return stream;
		try {
			const ctx = new AudioContext();
			const dest = ctx.createMediaStreamDestination();
			for (const track of tracks) {
				const src = ctx.createMediaStreamSource(new MediaStream([track]));
				src.connect(dest);
			}
			for (const track of tracks) {
				stream.removeTrack(track);
				track.stop();
			}
			for (const t of dest.stream.getAudioTracks()) {
				stream.addTrack(t);
			}
			await ctx.close();
		} catch {
			// fallback: keep only first audio track
			const keep = tracks[0];
			for (let i = 1; i < tracks.length; i++) {
				stream.removeTrack(tracks[i]);
				tracks[i].stop();
			}
		}
		return stream;
	}

	// ── Get display stream ──────────────────────────────────────────────────
	const getStream = useCallback(async (sourceId: string | null): Promise<MediaStream> => {
		const audioSource = settings?.audioSource ?? "desktop";

		// Tell main process which source to return when getDisplayMedia fires
		await window.clipsta?.setPendingSource(sourceId);

		const stream = await navigator.mediaDevices.getDisplayMedia({
			video: { frameRate: settings?.fps ?? 60 },
			audio: true, // handler provides "loopback"
		});

		// Remove system audio if not needed
		if (audioSource !== "desktop" && audioSource !== "both") {
			stream.getAudioTracks().forEach((t) => { stream.removeTrack(t); t.stop(); });
		}

		const auxStreams: MediaStream[] = [];

		if (audioSource === "mic" || audioSource === "both") {
			try {
				const micConstraints: MediaTrackConstraints = {
					echoCancellation: true,
					noiseSuppression: true,
				};
				if (settings?.audioInputDeviceId) {
					micConstraints.deviceId = { exact: settings.audioInputDeviceId };
				}
				const mic = await navigator.mediaDevices.getUserMedia({ audio: micConstraints });
				auxStreams.push(mic);
				mic.getAudioTracks().forEach((t: MediaStreamTrack) => stream.addTrack(t));
			} catch {
				// mic not available
			}
		}

		auxStreamsRef.current = auxStreams;
		await ensureSingleAudioTrack(stream);
		return stream;
	}, [settings]);

	// ── Start recording ────────────────────────────────────────────────────
	const startRecording = useCallback(async (sourceId: string | null = null) => {
		try {
			setState((s) => ({ ...s, error: null, status: "recording" }));
			const stream = await getStream(sourceId);
			streamRef.current = stream;
			bufferRef.current = [];
			durationRef.current = 0;

			const mimeType = ["video/webm;codecs=vp9,opus", "video/webm;codecs=vp8,opus", "video/webm"]
				.find((m) => MediaRecorder.isTypeSupported(m)) ?? "video/webm";

			const recorder = new MediaRecorder(stream, {
				mimeType,
				videoBitsPerSecond: (settings?.bitrate ?? 8000) * 1000,
			});
			recorderRef.current = recorder;

			recorder.ondataavailable = (e) => {
				if (e.data.size > 0) {
					bufferRef.current.push({ chunk: e.data, ts: Date.now() });
					const cutoff = Date.now() - 310_000;
					bufferRef.current = bufferRef.current.filter((c) => c.ts >= cutoff);
				}
			};

			recorder.onstop = () => {
				clearInterval(timerRef.current!);
				streamRef.current?.getTracks().forEach((t) => t.stop());
				auxStreamsRef.current.forEach((s) => s.getTracks().forEach((t) => t.stop()));
				auxStreamsRef.current = [];
			};

			recorder.start(1000 / CHUNKS_PER_SECOND);

			timerRef.current = setInterval(() => {
				durationRef.current++;
				setState((s) => ({ ...s, duration: durationRef.current }));
			}, 1000);

			window.clipsta?.setRecordingState(true);
		} catch (err: any) {
			setState((s) => ({ ...s, status: "idle", error: err.message ?? "Capture failed" }));
		}
	}, [getStream, settings]);

	// ── Stop recording ────────────────────────────────────────────────────
	const stopRecording = useCallback(() => {
		recorderRef.current?.stop();
		clearInterval(timerRef.current!);
		setState((s) => ({ ...s, status: "idle", duration: 0 }));
		window.clipsta?.setRecordingState(false);
	}, []);

	// ── Save last N seconds from rolling buffer ───────────────────────────
	const saveClip = useCallback(async (seconds: number): Promise<string | null> => {
		if (!bufferRef.current.length) return null;
		const wasRecording = state.status === "recording";
		setState((s) => ({ ...s, status: "saving" }));

		const cutoff = Date.now() - seconds * 1000;
		const chunks = bufferRef.current
			.filter((c) => c.ts >= cutoff)
			.map((c) => c.chunk);

		if (!chunks.length) {
			setState((s) => ({ ...s, status: wasRecording ? "recording" : s.status, error: "Not enough buffer yet" }));
			return null;
		}

		const mimeType = recorderRef.current?.mimeType ?? "video/webm";
		const blob = new Blob(chunks, { type: mimeType });
		const arrayBuffer = await blob.arrayBuffer();

		const now = new Date();
		const stamp = `${now.getFullYear()}${pad(now.getMonth() + 1)}${pad(now.getDate())}_${pad(now.getHours())}${pad(now.getMinutes())}${pad(now.getSeconds())}`;
		const label = seconds <= 60 ? "1min" : "5min";
		const fileName = `Clipsta_${label}_${stamp}.webm`;

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

	// ── Save full recording ───────────────────────────────────────────────
	const saveFullRecording = useCallback(async (): Promise<string | null> => {
		const result = await saveClip(durationRef.current + 5);
		stopRecording();
		return result;
	}, [saveClip, stopRecording]);

	// ── Toggle ────────────────────────────────────────────────────────────
	const toggleRecording = useCallback(
		async (sourceId: string | null = null) => {
			if (state.status === "recording") stopRecording();
			else await startRecording(sourceId);
		},
		[state.status, startRecording, stopRecording]
	);

	// ── Listen for hotkeys ──────────────────────────────────────────────────
	const toggleRef = useRef(toggleRecording);
	const saveRef = useRef(saveClip);
	toggleRef.current = toggleRecording;
	saveRef.current = saveClip;

	useEffect(() => {
		if (!window.clipsta) return;
		window.clipsta.onHotkeyRecord(() => toggleRef.current(sourceIdRef.current));
		window.clipsta.onHotkeyClip1Min(() => saveRef.current(60));
		window.clipsta.onHotkeyClip5Min(() => saveRef.current(300));
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

function pad(n: number) { return String(n).padStart(2, "0"); }
