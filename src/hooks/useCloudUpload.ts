import { useCallback, useEffect, useRef, useState } from "react";
import type { AppSettings, CloudConfig, UploadJob } from "../types";

interface CloudState {
	paired: boolean;
	pairingUrl: string | null;
	pairingCode: string | null;
	pairingError: string | null;
	pairingLoading: boolean;
	pairingConfirmed: boolean;
	queue: UploadJob[];
}

export function useCloudUpload(settings: AppSettings | null) {
	const [state, setState] = useState<CloudState>({
		paired: false,
		pairingUrl: null,
		pairingCode: null,
		pairingError: null,
		pairingLoading: false,
		pairingConfirmed: false,
		queue: [],
	});

	const queueRef = useRef<UploadJob[]>([]);
	const processingRef = useRef(false);
	const pairingDeviceIdRef = useRef<string | null>(null);
	const cloudConfigRef = useRef<CloudConfig | null>(null);

	const getCloudConfig = useCallback(async () => {
		if (!cloudConfigRef.current) {
			cloudConfigRef.current = await window.clipsta?.getCloudConfig() ?? {
				apiBase: "https://clipsta-api.godson594.workers.dev",
				apiKey: "32b28eac803a1b24c19e20665919eaeb7f1493d2b5e3f68be7944db6d9f01b96",
			};
		}
		return cloudConfigRef.current;
	}, []);

	const getDeviceId = useCallback(() => {
		let id = settings?.desktopDeviceId;
		if (!id) {
			if (!pairingDeviceIdRef.current) {
				pairingDeviceIdRef.current = `desktop_${crypto.randomUUID().replaceAll("-", "").slice(0, 12)}`;
			}
			id = pairingDeviceIdRef.current;
		}
		return id;
	}, [settings]);

	const syncQueue = useCallback((next: UploadJob[]) => {
		queueRef.current = next;
		setState((prev) => ({ ...prev, queue: next }));
	}, []);

	const updateJob = useCallback((id: string, patch: Partial<UploadJob>) => {
		syncQueue(queueRef.current.map((j) => (j.id === id ? { ...j, ...patch } : j)));
	}, [syncQueue]);

	// ── Pairing ───────────────────────────────────────────────────────────
	const generatePairingCode = useCallback(async () => {
		setState((prev) => ({ ...prev, pairingError: null, pairingLoading: true, pairingConfirmed: false }));
		const cfg = await getCloudConfig();
		try {
			const res = await fetch(`${cfg.apiBase}/pairing-tokens`, {
				method: "POST",
				headers: {
					"Content-Type": "application/json",
					"X-Clipsta-Test-Key": cfg.apiKey,
				},
				body: JSON.stringify({
					desktopDeviceId: getDeviceId(),
					desktopName: "Clipsta Desktop",
				}),
			});
			if (!res.ok) {
				const body = await res.json().catch(() => ({}));
				throw new Error((body as any).error ?? `HTTP ${res.status}`);
			}
			const data = (await res.json()) as { token: string; pairingUrl: string; expiresAt: string };
			setState((prev) => ({
				...prev,
				paired: true,
				pairingUrl: data.pairingUrl,
				pairingCode: data.token,
				pairingLoading: false,
			}));
			window.clipsta?.setSetting("cloudPairCode", data.token);
		} catch (e: any) {
			setState((prev) => ({
				...prev,
				pairingError: `Pairing failed: ${e.message ?? ""}`,
				pairingLoading: false,
			}));
		}
	}, [settings, getCloudConfig, getDeviceId]);

	// Mark pairing as confirmed (called when user dismisses QR modal after scanning)
	const confirmPairing = useCallback(() => {
		setState((prev) => ({ ...prev, pairingConfirmed: true }));
	}, []);

	// Resume pairing on mount if we have a stored token
	useEffect(() => {
		if (settings?.cloudEnabled && settings?.cloudPairCode) {
			setState((prev) => ({ ...prev, paired: true, pairingCode: settings.cloudPairCode! }));
		}
	}, [settings?.cloudEnabled, settings?.cloudPairCode]);

	// ── Upload ────────────────────────────────────────────────────────────
	const MAX_RETRIES = 5;
	const RETRY_DELAYS = [1000, 2000, 4000, 8000, 16000];

	const processQueue = useCallback(async () => {
		if (processingRef.current) return;
		processingRef.current = true;

		try {
			while (queueRef.current.some((j) => j.status === "queued")) {
				const job = queueRef.current.find((j) => j.status === "queued");
				if (!job) break;

				updateJob(job.id, { status: "uploading", progress: 0 });

				try {
					const result = await window.clipsta?.uploadClip({
						desktopDeviceId: getDeviceId(),
						filePath: job.path,
						fileName: job.name,
						durationSeconds: 30,
						bytes: job.size,
						capturedAt: new Date().toISOString(),
						encoder: settings?.encoder,
						trimStart: job.trimStart,
						trimEnd: job.trimEnd,
						cuts: job.cuts,
					});

					updateJob(job.id, {
						status: "done",
						progress: 100,
						streamUid: result?.streamUid,
						shareUrl: result?.shareUrl,
					});
				} catch (e: any) {
					const retryCount = (job.retryCount ?? 0) + 1;
					if (retryCount <= MAX_RETRIES) {
						const delay = RETRY_DELAYS[retryCount - 1] ?? 16000;
						updateJob(job.id, {
							status: "queued",
							retryCount,
							error: `Retry ${retryCount}/${MAX_RETRIES}: ${e.message ?? String(e)}`,
						});
						await new Promise((r) => setTimeout(r, delay));
					} else {
						updateJob(job.id, { status: "failed", error: e.message ?? String(e) });
					}
				}
			}
		} finally {
			processingRef.current = false;
			if (queueRef.current.some((j) => j.status === "queued")) {
				setTimeout(processQueue, 100);
			}
		}
	}, [settings, updateJob]);

	const clearPairing = useCallback(() => {
		setState((prev) => ({
			...prev,
			paired: false,
			pairingUrl: null,
			pairingCode: null,
			pairingError: null,
			pairingConfirmed: false,
		}));
		window.clipsta?.setSetting("cloudPairCode", "");
	}, []);

	const addToQueue = useCallback((path: string, name: string, size: number, trimOpts?: { trimStart?: number; trimEnd?: number; cuts?: { start: number; end: number }[] }) => {
		const existing = queueRef.current.find((j) => j.path === path);
		if (existing) {
			if (existing.status === "queued" || existing.status === "uploading") return;
			updateJob(existing.id, { status: "queued", progress: 0, error: undefined, name, retryCount: 0, ...trimOpts });
			setTimeout(processQueue, 100);
			return;
		}
		const id = `${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
		syncQueue([...queueRef.current, { id, path, name, size, progress: 0, status: "queued", ...trimOpts }]);
		setTimeout(processQueue, 100);
	}, [syncQueue, updateJob, processQueue]);

	const retryJob = useCallback((id: string) => {
		updateJob(id, { status: "queued", progress: 0, error: undefined, retryCount: 0 });
		setTimeout(processQueue, 100);
	}, [updateJob, processQueue]);

	const removeJob = useCallback((id: string) => {
		syncQueue(queueRef.current.filter((j) => j.id !== id));
	}, [syncQueue]);

	// ── Upload status notifications ───────────────────────────────────────
	const notifyStatus = useCallback(() => {
		const queue = queueRef.current;
		const queuedCount = queue.filter((j) => j.status === "queued").length;
		const uploadingCount = queue.filter((j) => j.status === "uploading").length;
		const uploadedCount = queue.filter((j) => j.status === "done").length;
		const failedCount = queue.filter((j) => j.status === "failed").length;
		const uploading = queue.find((j) => j.status === "uploading");
		const currentProgressPercent = uploading?.progress ?? 0;
		const parts: string[] = [];
		if (queuedCount) parts.push(`${queuedCount} queued`);
		if (uploadingCount) parts.push(`${uploadingCount} uploading (${currentProgressPercent}%)`);
		if (uploadedCount) parts.push(`${uploadedCount} done`);
		if (failedCount) parts.push(`${failedCount} failed`);
		const currentStatus = parts.length ? parts.join(", ") : "Idle";

		window.clipsta?.notifyUploadStatus({
			desktopDeviceId: getDeviceId(),
			desktopName: "Clipsta Desktop",
			queuedCount,
			waitingForGameplayCount: 0,
			uploadingCount,
			uploadedCount,
			failedCount,
			currentProgressPercent,
			currentStatus,
		}).catch(() => {});
	}, [settings]);

	useEffect(() => {
		if (settings?.cloudEnabled) notifyStatus();
	}, [state.queue, settings?.cloudEnabled, notifyStatus]);

	return {
		...state,
		generatePairingCode,
		confirmPairing,
		clearPairing,
		addToQueue,
		retryJob,
		removeJob,
	};
}
