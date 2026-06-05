import { useCallback, useEffect, useRef, useState } from "react";
import type { AppSettings, UploadJob } from "../types";

const API_BASE = "https://clipsta-api.godson594.workers.dev";

interface CloudState {
	paired: boolean;
	pairingUrl: string | null;
	pairingCode: string | null;
	pairingError: string | null;
	pairingLoading: boolean;
	queue: UploadJob[];
}

export function useCloudUpload(settings: AppSettings | null) {
	const [state, setState] = useState<CloudState>({
		paired: false,
		pairingUrl: null,
		pairingCode: null,
		pairingError: null,
		pairingLoading: false,
		queue: [],
	});

	const queueRef = useRef<UploadJob[]>([]);
	const processingRef = useRef(false);
	const pairingDeviceIdRef = useRef<string | null>(null);

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
		setState((prev) => ({ ...prev, pairingError: null, pairingLoading: true }));
		try {
			const res = await fetch(`${API_BASE}/pairing-tokens`, {
				method: "POST",
				headers: {
					"Content-Type": "application/json",
					"X-Clipsta-Test-Key": "dev-clipsta",
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
	}, [settings]);

	// Resume pairing on mount if we have a stored token
	useEffect(() => {
		if (settings?.cloudEnabled && settings?.cloudPairCode) {
			setState((prev) => ({ ...prev, paired: true, pairingCode: settings.cloudPairCode! }));
		}
	}, [settings?.cloudEnabled, settings?.cloudPairCode]);

	// ── Upload ────────────────────────────────────────────────────────────
	const processQueue = useCallback(async () => {
		if (processingRef.current) return;
		processingRef.current = true;

		while (queueRef.current.some((j) => j.status === "queued")) {
			const job = queueRef.current.find((j) => j.status === "queued");
			if (!job) break;

			updateJob(job.id, { status: "uploading", progress: 0 });

			try {
				const result = await window.clipsta?.uploadClip({
					desktopDeviceId: getDeviceId(),
					filePath: job.path,
					fileName: job.name,
					durationSeconds: 0,
					bytes: job.size,
					capturedAt: new Date().toISOString(),
				});

				updateJob(job.id, {
					status: "done",
					progress: 100,
					streamUid: result?.streamUid,
					shareUrl: result?.shareUrl,
				});
			} catch (e: any) {
				updateJob(job.id, { status: "failed", error: e.message ?? String(e) });
			}
		}

		processingRef.current = false;
	}, [settings, updateJob]);

	const addToQueue = useCallback((path: string, name: string, size: number) => {
		const id = `${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
		syncQueue([...queueRef.current, { id, path, name, size, progress: 0, status: "queued" }]);
		setTimeout(processQueue, 100);
	}, [syncQueue, processQueue]);

	const retryJob = useCallback((id: string) => {
		updateJob(id, { status: "queued", progress: 0, error: undefined });
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
		addToQueue,
		retryJob,
		removeJob,
	};
}
