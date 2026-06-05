import { useCallback, useEffect, useRef, useState } from "react";
import type { AppSettings, UploadJob } from "../types";

const API_BASE = "https://clipsta-api.godson594.workers.dev";

interface CloudState {
	paired: boolean;
	pairingCode: string | null;
	pairingError: string | null;
	pairingLoading: boolean;
	queue: UploadJob[];
}

export function useCloudUpload(settings: AppSettings | null) {
	const [state, setState] = useState<CloudState>({
		paired: false,
		pairingCode: null,
		pairingError: null,
		pairingLoading: false,
		queue: [],
	});

	const queueRef = useRef<UploadJob[]>([]);
	const processingRef = useRef(false);

	// Sync ref with state
	const setQueue = useCallback((fn: (prev: UploadJob[]) => UploadJob[]) => {
		setState((prev) => {
			const next = fn(prev.queue);
			queueRef.current = next;
			return { ...prev, queue: next };
		});
	}, []);

	const updateJob = useCallback((id: string, patch: Partial<UploadJob>) => {
		setQueue((prev) => prev.map((j) => (j.id === id ? { ...j, ...patch } : j)));
	}, [setQueue]);

	// ── Pairing ───────────────────────────────────────────────────────────
	const generatePairingCode = useCallback(async () => {
		if (!settings?.cloudEnabled) return;
		setState((prev) => ({ ...prev, pairingError: null, pairingLoading: true }));
		try {
			const res = await fetch(`${API_BASE}/desktop/pair-code`, {
				method: "POST",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({ deviceName: "Clipsta Desktop" }),
			});
			if (!res.ok) {
				const body = await res.json().catch(() => ({}));
				throw new Error((body as any).error ?? `HTTP ${res.status}`);
			}
			const data = (await res.json()) as { code: string; token: string };
			setState((prev) => ({ ...prev, paired: true, pairingCode: data.code, pairingLoading: false }));
			window.clipsta?.setSetting("cloudPairCode", data.token);
		} catch (e: any) {
			// Generate a local code as fallback so QR code always appears
			const localCode = Math.random().toString(36).toUpperCase().slice(2, 8);
			setState((prev) => ({
				...prev,
				pairingCode: localCode,
				pairingError: `Could not reach server — using local code. ${e.message ?? ""}`,
				pairingLoading: false,
			}));
		}
	}, [settings]);

	// Resume pairing on mount if we have a stored token
	useEffect(() => {
		if (settings?.cloudEnabled && settings?.cloudPairCode) {
			setState((prev) => ({ ...prev, paired: true }));
		}
	}, [settings?.cloudEnabled, settings?.cloudPairCode]);

	// ── Upload with bandwidth throttle ────────────────────────────────────
	const processQueue = useCallback(async () => {
		if (processingRef.current) return;
		processingRef.current = true;

		const maxBps = (settings?.uploadBandwidth ?? 0) * 1024; // KB/s → bytes/s

		while (queueRef.current.some((j) => j.status === "queued")) {
			const job = queueRef.current.find((j) => j.status === "queued");
			if (!job) break;

			updateJob(job.id, { status: "uploading", progress: 0 });

			try {
				const fileBuf = await window.clipsta?.readFile(job.path);
				if (!fileBuf) throw new Error("Could not read file");

				// Upload with bandwidth throttle
				const xhr = new XMLHttpRequest();
				xhr.open("POST", `${API_BASE}/desktop/upload`);
				xhr.setRequestHeader("Authorization", `Bearer ${settings?.cloudPairCode ?? ""}`);
				xhr.setRequestHeader("X-Filename", encodeURIComponent(job.name));

				await new Promise<void>((resolve, reject) => {
					xhr.upload.onprogress = (e) => {
						if (e.lengthComputable) {
							const pct = Math.round((e.loaded / e.total) * 100);
							updateJob(job.id, { progress: pct });

							// Bandwidth throttle
							if (maxBps > 0) {
								const elapsed = (Date.now() - (xhr as any)._startTime) / 1000;
								const expected = e.loaded / maxBps;
								if (elapsed < expected) {
									const delay = (expected - elapsed) * 1000;
									// Sleep using setTimeout — this blocks the chunk callback from being called too fast
									const t0 = Date.now();
									while (Date.now() - t0 < delay) {
										// busy-wait for small delays
									}
								}
							}
						}
					};
					(xhr as any)._startTime = Date.now();
					xhr.onload = () => {
						if (xhr.status >= 200 && xhr.status < 300) resolve();
						else reject(new Error(`Upload failed: HTTP ${xhr.status}`));
					};
					xhr.onerror = () => reject(new Error("Upload network error"));
					xhr.send(fileBuf);
				});

				updateJob(job.id, { status: "done", progress: 100 });
			} catch (e: any) {
				updateJob(job.id, { status: "failed", error: e.message ?? String(e) });
			}
		}

		processingRef.current = false;
	}, [settings, updateJob]);

	const addToQueue = useCallback((path: string, name: string, size: number) => {
		const id = `${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
		setQueue((prev) => [...prev, { id, path, name, size, progress: 0, status: "queued" }]);
		// Trigger processing
		setTimeout(processQueue, 100);
	}, [setQueue, processQueue]);

	const retryJob = useCallback((id: string) => {
		updateJob(id, { status: "queued", progress: 0, error: undefined });
		setTimeout(processQueue, 100);
	}, [updateJob, processQueue]);

	const removeJob = useCallback((id: string) => {
		setQueue((prev) => prev.filter((j) => j.id !== id));
	}, [setQueue]);

	return {
		...state,
		generatePairingCode,
		addToQueue,
		retryJob,
		removeJob,
	};
}
