import { useCallback, useEffect, useRef, useState } from "react";
import type { AppSettings, CloudConfig, UploadJob } from "../types";
import bridge from "../tauri-bridge";

interface CloudState {
	paired: boolean;
	pairingUrl: string | null;
	pairingCode: string | null;
	pairingError: string | null;
	pairingLoading: boolean;
	pairingConfirmed: boolean;
	queue: UploadJob[];
}

/**
 * Structured classification of an upload error message into permanent vs retryable.
 *
 * Replaces the old `msg.includes("HTTP 4")` heuristic. The Rust backend formats
 * failures as e.g. "Upload: HTTP 403" / "Cloud API error: HTTP 500", and already
 * retries transient cases (network, 5xx, 429) before surfacing anything here.
 *
 * Permanent (do NOT retry in the frontend):
 *   - local/terminal conditions: "too large", "Not paired", "No uploadUrl", "Parse error", "empty"
 *   - HTTP 4xx client errors EXCEPT 429 (rate limit is transient)
 * Everything else is treated as retryable (last-resort frontend safety net).
 */
function isPermanentUploadError(msg: string): boolean {
	const terminal = [
		"too large",
		"Not paired",
		"No uploadUrl",
		"Parse error",
		"empty or doesn't exist",
	];
	if (terminal.some((t) => msg.includes(t))) return true;

	// Extract an HTTP status code if present, e.g. "HTTP 403".
	const m = msg.match(/HTTP\s+(\d{3})/);
	if (m) {
		const code = Number(m[1]);
		if (code === 429) return false; // rate-limited → transient
		if (code >= 400 && code < 500) return true; // other client errors → permanent
		// 5xx (and anything else) → transient
		return false;
	}
	// No status code (network error, unknown) → treat as transient/retryable.
	return false;
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
			try {
				cloudConfigRef.current = await bridge.getCloudConfig();
			} catch {
				cloudConfigRef.current = {
					apiBase: "https://clipsta-api.godson594.workers.dev",
				};
			}
		}
		return cloudConfigRef.current;
	}, []);

	const getDeviceId = useCallback(() => {
		let id = settings?.desktopDeviceId;
		if (!id) {
			if (!pairingDeviceIdRef.current) {
				pairingDeviceIdRef.current = `desktop_${crypto.randomUUID().replace(/-/g, "").slice(0, 12)}`;
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
		try {
			// Use backend proxy — API key stays server-side
			const { invoke } = await import("@tauri-apps/api/core");
			const data = await invoke<{ token: string; pairingUrl: string; expiresAt: string }>("cloud_generate_pairing");
			setState((prev) => ({
				...prev,
				paired: true,
				pairingUrl: data.pairingUrl,
				pairingCode: data.token,
				pairingLoading: false,
			}));
		} catch (e: any) {
			setState((prev) => ({
				...prev,
				pairingError: `Pairing failed: ${e?.message ?? e ?? ""}`,
				pairingLoading: false,
			}));
		}
	}, []);

	const confirmPairing = useCallback(() => {
		setState((prev) => ({ ...prev, pairingConfirmed: true }));
	}, []);

	// Resume pairing on mount if we have a stored token
	useEffect(() => {
		if (settings?.cloudEnabled && settings?.cloudPairCode) {
			setState((prev) => ({ ...prev, paired: true, pairingConfirmed: true, pairingCode: settings.cloudPairCode! }));
		}
	}, [settings?.cloudEnabled, settings?.cloudPairCode]);

	// ── Upload ────────────────────────────────────────────────────────────
	const MAX_RETRIES = 5;
	const RETRY_DELAYS = [1000, 2000, 4000, 8000, 16000];
	const MAX_DONE_JOBS = 50;

	const settingsRef = useRef(settings);
	settingsRef.current = settings;
	const getDeviceIdRef = useRef(getDeviceId);
	getDeviceIdRef.current = getDeviceId;
	const updateJobRef = useRef(updateJob);
	updateJobRef.current = updateJob;
	const syncQueueRef = useRef(syncQueue);
	syncQueueRef.current = syncQueue;

	const processQueue = useCallback(async () => {
		if (processingRef.current) return;
		processingRef.current = true;

		try {
			while (true) {
				const job = queueRef.current.find((j) => j.status === "queued");
				if (!job) break;

				// Check if cloud is paired before attempting upload
				const pairCode = settingsRef.current?.cloudPairCode;
				if (!pairCode) {
					updateJobRef.current(job.id, { status: "failed", error: "Not paired — pair device in Settings first" });
					continue;
				}

				updateJobRef.current(job.id, { status: "uploading", progress: 0 });

				try {
					// Real progress arrives via the "upload:progress" Tauri event
					// (wired in the effect below), keyed by the clip's file path.
					// The Rust backend also does its own bounded retry with backoff
					// for transient failures and streams the file (no 120s abort),
					// so we no longer race against a frontend timeout that couldn't
					// cancel the backend anyway.
					const result = await bridge.uploadClip({
						desktopDeviceId: getDeviceIdRef.current(),
						filePath: job.path,
						fileName: job.name,
						durationSeconds: job.durationSeconds,
						bytes: job.size,
						capturedAt: new Date().toISOString(),
						encoder: settingsRef.current?.encoder,
						trimStart: job.trimStart,
						trimEnd: job.trimEnd,
						cuts: job.cuts,
					});

					updateJobRef.current(job.id, {
						status: "done",
						progress: 100,
						streamUid: result?.streamUid,
						shareUrl: result?.shareUrl,
					});
				} catch (e: any) {
					const msg = e?.message ?? String(e ?? "Unknown upload error");
					const retryCount = (job.retryCount ?? 0) + 1;

					// Structured permanent-error detection (replaces the fragile
					// `msg.includes("HTTP 4")` check). The backend already exhausted
					// its own transient retries before surfacing an error here, so a
					// frontend retry is a last-resort safety net. Treat client errors
					// (4xx except 429) and known-terminal conditions as permanent.
					const permanent = isPermanentUploadError(msg);
					if (!permanent && retryCount <= MAX_RETRIES) {
						const delay = RETRY_DELAYS[retryCount - 1] ?? 16000;
						updateJobRef.current(job.id, {
							status: "queued",
							progress: 0,
							retryCount,
							error: `Retry ${retryCount}/${MAX_RETRIES}: ${msg}`,
						});
						await new Promise((r) => setTimeout(r, delay));
					} else {
						updateJobRef.current(job.id, { status: "failed", error: msg });
					}
				}
			}
		} catch (outerErr) {
			console.error("[useCloudUpload] processQueue error:", outerErr);
		} finally {
			processingRef.current = false;
		}
	}, []);

	const triggerProcessQueue = useCallback(() => {
		setTimeout(() => processQueue(), 100);
	}, [processQueue]);

	// Watchdog
	const lastProgressRef = useRef(Date.now());
	useEffect(() => {
		const interval = setInterval(() => {
			const hasQueued = queueRef.current.some((j) => j.status === "queued");
			const hasUploading = queueRef.current.some((j) => j.status === "uploading");

			if (hasQueued && !hasUploading && !processingRef.current) {
				processQueue();
				lastProgressRef.current = Date.now();
			} else if (hasQueued && processingRef.current && !hasUploading) {
				const stuckMs = Date.now() - lastProgressRef.current;
				if (stuckMs > 30000) {
					console.warn("[useCloudUpload] watchdog: stuck for 30s, resetting");
					processingRef.current = false;
					lastProgressRef.current = Date.now();
					setTimeout(() => processQueue(), 500);
				}
			} else {
				lastProgressRef.current = Date.now();
			}
		}, 5000);
		return () => clearInterval(interval);
	}, [processQueue]);

	// ── Real upload progress ──────────────────────────────────────────────
	// The Rust backend streams the file and emits "upload:progress" events
	// (keyed by the clip's file path) as bytes go out the socket, plus
	// "upload:retry" on transient backoff retries. Match by path → job id.
	useEffect(() => {
		let unlistenProgress: (() => void) | undefined;
		let unlistenRetry: (() => void) | undefined;
		let cancelled = false;

		bridge.onUploadProgress((p) => {
			const job = queueRef.current.find((j) => j.path === p.id);
			if (job && job.status === "uploading") {
				const pct = Math.max(0, Math.min(100, Math.round(p.percent)));
				updateJobRef.current(job.id, { progress: pct });
				lastProgressRef.current = Date.now();
			}
		}).then((fn) => {
			if (cancelled) fn();
			else unlistenProgress = fn;
		}).catch(() => {});

		bridge.onUploadRetry((p) => {
			const job = queueRef.current.find((j) => j.path === p.id);
			if (job) {
				updateJobRef.current(job.id, {
					progress: 0,
					error: `Retrying (${p.attempt}/${p.maxAttempts}): ${p.message}`,
				});
				lastProgressRef.current = Date.now();
			}
		}).then((fn) => {
			if (cancelled) fn();
			else unlistenRetry = fn;
		}).catch(() => {});

		return () => {
			cancelled = true;
			unlistenProgress?.();
			unlistenRetry?.();
		};
	}, []);

	const clearPairing = useCallback(() => {
		setState((prev) => ({
			...prev,
			paired: false,
			pairingUrl: null,
			pairingCode: null,
			pairingError: null,
			pairingConfirmed: false,
		}));
		bridge.setSetting("cloudPairCode", "").catch(() => {});
	}, []);

	const addToQueue = useCallback((path: string, name: string, size: number, trimOpts?: { trimStart?: number; trimEnd?: number; cuts?: { start: number; end: number }[] }) => {
		const existing = queueRef.current.find((j) => j.path === path);
		if (existing) {
			// Allow re-upload if done, failed, or stuck
			if (existing.status === "uploading") return; // Don't interrupt active upload
			if (existing.status === "queued") {
				// Already queued — just trigger processing
				triggerProcessQueue();
				return;
			}
			// Reset done/failed status to re-upload
			updateJob(existing.id, { status: "queued" as const, progress: 0, error: undefined, name, retryCount: 0, ...trimOpts });
			triggerProcessQueue();
			return;
		}
		const id = `${Date.now()}_${Math.random().toString(36).slice(2, 8)}`;
		const newJob: UploadJob = { id, path, name, size, progress: 0, status: "queued", ...trimOpts };
		const next = [...queueRef.current, newJob];
		const doneFailed = next.filter((j) => j.status === "done" || j.status === "failed");
		if (doneFailed.length > MAX_DONE_JOBS) {
			const keepIds = new Set(doneFailed.slice(-MAX_DONE_JOBS).map((j) => j.id));
			syncQueue(next.filter((j) => j.status === "queued" || j.status === "uploading" || keepIds.has(j.id)));
		} else {
			syncQueue(next);
		}
		triggerProcessQueue();
	}, [syncQueue, updateJob, triggerProcessQueue]);

	const retryJob = useCallback((id: string) => {
		updateJob(id, { status: "queued", progress: 0, error: undefined, retryCount: 0 });
		triggerProcessQueue();
	}, [updateJob, triggerProcessQueue]);

	const removeJob = useCallback((id: string) => {
		syncQueue(queueRef.current.filter((j) => j.id !== id));
	}, [syncQueue]);

	// Upload status notifications
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

		bridge.notifyUploadStatus({
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
	}, [getDeviceId]);

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
