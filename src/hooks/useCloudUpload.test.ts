import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { renderHook, act } from "@testing-library/react";
import type { AppSettings } from "../types";

// Mock window.clipsta before importing the hook
const mockUploadClip = vi.fn();
const mockGetCloudConfig = vi.fn();
const mockSetSetting = vi.fn();
const mockNotifyUploadStatus = vi.fn().mockResolvedValue(undefined);

(globalThis as any).window = globalThis;
(globalThis as any).clipsta = {
	uploadClip: mockUploadClip,
	getCloudConfig: mockGetCloudConfig,
	setSetting: mockSetSetting,
	notifyUploadStatus: mockNotifyUploadStatus,
};

import { useCloudUpload } from "./useCloudUpload";

function makeSettings(overrides?: Partial<AppSettings>): AppSettings {
	return {
		outputFolder: "C:\\clips",
		hotkeyClip1Min: "Alt+F9",
		hotkeyClip5Min: "Alt+F10",
		hotkeyRecord: "F9",
		bufferDuration: 60,
		resolution: "1080p",
		fps: 60,
		aspectRatio: "16:9",
		encoder: "auto",
		bitrate: 50000,
		audioBitrate: 192,
		captureAudio: true,
		captureMic: false,
		audioSource: "desktop",
		audioInputDeviceId: "",
		gameDetect: true,
		autoUpload: false,
		minimizeToTray: true,
		overlayEnabled: true,
		cloudEnabled: true,
		cloudPairCode: "",
		uploadBandwidth: 0,
		deleteAfterUpload: false,
		desktopDeviceId: "test_device_123",
		...overrides,
	};
}

describe("useCloudUpload", () => {
	beforeEach(() => {
		vi.clearAllMocks();
		vi.useFakeTimers();
		mockGetCloudConfig.mockResolvedValue({
			apiBase: "https://test.api",
			apiKey: "test-key",
		});
		mockUploadClip.mockResolvedValue({
			id: "stream_1",
			streamUid: "stream_1",
			uploadUrl: "https://upload.example.com/test",
		});
	});

	afterEach(() => {
		vi.clearAllTimers();
		vi.useRealTimers();
	});

	// ── Initial state ──
	it("starts unpaired with empty queue", () => {
		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		expect(result.current.paired).toBe(false);
		expect(result.current.queue).toEqual([]);
	});

	// ── addToQueue ──
	it("adds a job to the queue", () => {
		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		act(() => {
			result.current.addToQueue("C:\\clip.mp4", "clip.mp4", 1024);
		});
		expect(result.current.queue).toHaveLength(1);
		expect(result.current.queue[0].path).toBe("C:\\clip.mp4");
		expect(result.current.queue[0].status).toBe("queued");
	});

	it("does not duplicate a queued job for the same path", () => {
		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		act(() => {
			result.current.addToQueue("C:\\clip.mp4", "clip.mp4", 1024);
		});
		act(() => {
			result.current.addToQueue("C:\\clip.mp4", "clip.mp4", 1024);
		});
		expect(result.current.queue).toHaveLength(1);
	});

	it("re-queues a failed job", () => {
		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		act(() => {
			result.current.addToQueue("C:\\clip.mp4", "clip.mp4", 1024);
		});
		act(() => { vi.advanceTimersByTime(200); });
		expect(mockUploadClip).toHaveBeenCalledTimes(1);
	});

	it("stores trim opts on the job", () => {
		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		act(() => {
			result.current.addToQueue("C:\\clip.mp4", "clip.mp4", 1024, {
				trimStart: 10,
				trimEnd: 50,
				cuts: [{ start: 20, end: 25 }],
			});
		});
		const job = result.current.queue[0];
		expect(job.trimStart).toBe(10);
		expect(job.trimEnd).toBe(50);
		expect(job.cuts).toEqual([{ start: 20, end: 25 }]);
	});

	// ── removeJob ──
	it("removeJob removes the job from queue", () => {
		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		act(() => {
			result.current.addToQueue("C:\\clip.mp4", "clip.mp4", 1024);
		});
		const id = result.current.queue[0].id;
		act(() => {
			result.current.removeJob(id);
		});
		expect(result.current.queue).toHaveLength(0);
	});

	// ── Upload flow ──
	it("processes a queued job to completion", async () => {
		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		act(() => {
			result.current.addToQueue("C:\\clip.mp4", "clip.mp4", 1024);
		});

		await act(async () => {
			await vi.advanceTimersByTimeAsync(200);
		});

		expect(mockUploadClip).toHaveBeenCalledTimes(1);
		expect(mockUploadClip).toHaveBeenCalledWith(
			expect.objectContaining({
				filePath: "C:\\clip.mp4",
				fileName: "clip.mp4",
				bytes: 1024,
			})
		);

		const doneJobs = result.current.queue.filter((j) => j.status === "done");
		expect(doneJobs).toHaveLength(1);
	});

	it("auto-retries on failure up to MAX_RETRIES", async () => {
		mockUploadClip.mockRejectedValue(new Error("Network error"));

		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		act(() => {
			result.current.addToQueue("C:\\clip.mp4", "clip.mp4", 1024);
		});

		// processQueue fires after 100ms
		await act(async () => {
			await vi.advanceTimersByTimeAsync(200);
		});

		// Retry 1: delay 1s
		await act(async () => {
			await vi.advanceTimersByTimeAsync(1100);
		});
		// Retry 2: delay 2s
		await act(async () => {
			await vi.advanceTimersByTimeAsync(2100);
		});
		// Retry 3: delay 4s
		await act(async () => {
			await vi.advanceTimersByTimeAsync(4100);
		});
		// Retry 4: delay 8s
		await act(async () => {
			await vi.advanceTimersByTimeAsync(8100);
		});
		// Retry 5: delay 16s
		await act(async () => {
			await vi.advanceTimersByTimeAsync(16100);
		});

		// 1 initial + 5 retries = 6 total attempts
		expect(mockUploadClip).toHaveBeenCalledTimes(6);

		const failedJobs = result.current.queue.filter((j) => j.status === "failed");
		expect(failedJobs).toHaveLength(1);
	});

	// ── generatePairingCode ──
	it("generatePairingCode calls the pairing API", async () => {
		const mockFetch = vi.fn().mockResolvedValue({
			ok: true,
			json: async () => ({
				token: "pair_token_123",
				pairingUrl: "clipsta://pair?token=...",
				expiresAt: new Date(Date.now() + 600000).toISOString(),
			}),
		});
		vi.stubGlobal("fetch", mockFetch);

		const { result } = renderHook(() => useCloudUpload(makeSettings()));

		await act(async () => {
			await result.current.generatePairingCode();
		});

		expect(mockFetch).toHaveBeenCalledWith(
			"https://test.api/pairing-tokens",
			expect.objectContaining({ method: "POST" })
		);
		expect(result.current.paired).toBe(true);
		expect(result.current.pairingCode).toBe("pair_token_123");

		vi.unstubAllGlobals();
	});

	// ── confirmPairing / clearPairing ──
	it("confirmPairing sets pairingConfirmed", () => {
		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		act(() => { result.current.confirmPairing(); });
		expect(result.current.pairingConfirmed).toBe(true);
	});

	it("clearPairing resets pairing state", () => {
		const { result } = renderHook(() => useCloudUpload(makeSettings()));
		act(() => { result.current.confirmPairing(); });
		act(() => { result.current.clearPairing(); });
		expect(result.current.paired).toBe(false);
		expect(result.current.pairingConfirmed).toBe(false);
	});
});
