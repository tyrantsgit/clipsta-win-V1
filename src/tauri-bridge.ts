/**
 * Tauri Bridge — replaces Electron's preload/contextBridge.
 * Maps every window.clipsta.* method to Tauri invoke() or event listen().
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open, save } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import type {
	AppSettings,
	AudioDevice,
	ClipFile,
	CloudConfig,
	ExportOpts,
	FileStats,
	Mp4Info,
	ScreenSource,
	SystemInfo,
	TrimResult,
	UploadClipOpts,
	UploadClipResponse,
	UploadStatusBody,
	WgcSaveClipOpts,
	WgcSource,
	WgcStartOpts,
	WgcStartResult,
} from "./types";

// ── Window Controls ─────────────────────────────────────────────────────────
export function minimize() {
	getCurrentWindow().minimize();
}

export function maximize() {
	getCurrentWindow().toggleMaximize();
}

export function close() {
	getCurrentWindow().close();
}

// ── Sources ─────────────────────────────────────────────────────────────────
export function getSources(): Promise<ScreenSource[]> {
	return invoke<ScreenSource[]>("wgc_sources");
}

export function getWgcSources(): Promise<WgcSource[]> {
	return invoke<WgcSource[]>("wgc_sources");
}

// ── Recording ───────────────────────────────────────────────────────────────
export function setPendingSource(_id: string | null): Promise<void> {
	// No-op in Tauri — not needed (WGC source set via start_recording)
	return Promise.resolve();
}

export function setRecordingState(_recording: boolean): Promise<void> {
	// No-op in Tauri — recording state is managed by the Rust CaptureSession internally
	return Promise.resolve();
}

export function saveRecording(_buf: ArrayBuffer, _name: string): Promise<string> {
	// No-op in Tauri — recording is saved via wgc_save_clip
	return Promise.resolve("");
}

export function exportRecording(inputPath: string, outputPath: string, opts: ExportOpts): Promise<string> {
	return invoke<string>("recording_export", { inputPath, outputPath, opts });
}

export async function browseSaveExport(defaultName: string): Promise<string | null> {
	const result = await save({
		defaultPath: defaultName,
		filters: [
			{ name: "Video", extensions: ["mp4", "webm", "mkv", "mov"] },
		],
	});
	return result ?? null;
}

// ── WGC Recording ───────────────────────────────────────────────────────────
export function wgcStartRecording(opts: WgcStartOpts): Promise<WgcStartResult | null> {
	return invoke<WgcStartResult | null>("wgc_start_recording", { opts });
}

export function wgcSaveClip(opts: WgcSaveClipOpts): Promise<string | null> {
	return invoke<string | null>("wgc_save_clip", { seconds: opts.seconds, fileName: opts.fileName });
}

export function wgcStopRecording(): Promise<void> {
	return invoke("wgc_stop_recording");
}

export function wgcPauseToggle(): Promise<void> {
	// Not implemented in Tauri version
	return Promise.resolve();
}

export function wgcSaveFullRecording(): Promise<string | null> {
	return invoke<string | null>("wgc_save_full_recording");
}

export function onWgcClipSaved(cb: (path: string) => void): UnlistenFn | Promise<UnlistenFn> {
	return listen<string>("wgc:clipSaved", (event) => cb(event.payload));
}

export function onWgcLog(cb: (level: string, line: string) => void): UnlistenFn | Promise<UnlistenFn> {
	return listen<{ level: string; line: string }>("wgc-log", (event) => {
		cb(event.payload.level, event.payload.line);
	});
}

// ── Hotkey Events ───────────────────────────────────────────────────────────
// Rust emits "hotkey:clip" with seconds (30/60/300) as payload
export function onHotkeyRecord(cb: () => void): Promise<UnlistenFn> {
	return listen("hotkey:record", () => cb());
}

export function onHotkeyClip30Sec(cb: () => void): Promise<UnlistenFn> {
	return listen<number>("hotkey:clip", (event) => {
		if (event.payload === 30) cb();
	});
}

export function onHotkeyClip1Min(cb: () => void): Promise<UnlistenFn> {
	return listen<number>("hotkey:clip", (event) => {
		if (event.payload === 60) cb();
	});
}

export function onHotkeyClip5Min(cb: () => void): Promise<UnlistenFn> {
	return listen<number>("hotkey:clip", (event) => {
		if (event.payload === 300) cb();
	});
}

export function onPlayClipSound(cb: () => void): Promise<UnlistenFn> {
	return listen("play-clip-sound", () => cb());
}

// ── Settings ────────────────────────────────────────────────────────────────
export function getSettings(): Promise<AppSettings> {
	return invoke<AppSettings>("settings_get_all");
}

export function setSetting(key: string, value: unknown): Promise<boolean> {
	return invoke<boolean>("settings_set", { key, value });
}

export function setAllSettings(s: Partial<AppSettings>): Promise<boolean> {
	return invoke<boolean>("settings_set_all", { settings: s });
}

export function suspendHotkeys(): Promise<boolean> {
	return invoke<boolean>("hotkeys_suspend");
}

export function resumeHotkeys(): Promise<boolean> {
	return invoke<boolean>("hotkeys_resume");
}

// ── Dialogs ─────────────────────────────────────────────────────────────────
export async function browseFolder(): Promise<string | null> {
	const result = await open({ directory: true, multiple: false });
	return result ?? null;
}

export async function browseImportFolder(): Promise<string | null> {
	const result = await open({ directory: true, multiple: false, title: "Select folder to import" });
	return result ?? null;
}

export async function browseFile(): Promise<string | null> {
	const result = await open({
		multiple: false,
		filters: [{ name: "Video", extensions: ["mp4", "webm", "mkv", "mov"] }],
	});
	return result ?? null;
}

// ── Shell ───────────────────────────────────────────────────────────────────
export async function openFolder(p: string): Promise<void> {
	await openPath(p);
}

export async function openFile(p: string): Promise<void> {
	await openPath(p);
}

export function showInFolder(p: string): Promise<void> {
	return invoke("shell_show_item", { path: p });
}

// ── Audio Devices ───────────────────────────────────────────────────────────
export function listAudioDevices(): Promise<AudioDevice[]> {
	return invoke<AudioDevice[]>("audio_list_devices");
}

export function getDefaultAudioDevices(): Promise<{ defaultOutputId: string; defaultInputId: string }> {
	return invoke<{ defaultOutputId: string; defaultInputId: string }>("audio_default_devices");
}

// ── Clips ───────────────────────────────────────────────────────────────────
export function listClips(): Promise<ClipFile[]> {
	return invoke<ClipFile[]>("clips_list");
}

export function deleteClip(path: string): Promise<boolean> {
	return invoke<boolean>("clips_delete", { path });
}

export function renameClip(oldPath: string, newName: string): Promise<string> {
	return invoke<string>("clips_rename", { oldPath, newName });
}

export function importClip(sourcePath: string): Promise<string> {
	return invoke<string>("clips_import", { sourcePath });
}

export function importFolder(folderPath: string): Promise<string[]> {
	return invoke<string[]>("clips_import_folder", { folderPath });
}

// ── System ──────────────────────────────────────────────────────────────────
export function getSystemInfo(): Promise<SystemInfo> {
	return invoke<SystemInfo>("system_info");
}

/** Get the title of the active foreground window (for ShadowPlay-style game detection) */
export function getActiveWindowTitle(): Promise<string> {
	return invoke<string>("get_active_window_title");
}

// ── File Operations ─────────────────────────────────────────────────────────
export function readFile(_path: string): Promise<ArrayBuffer> {
	// Not directly needed — use Tauri fs plugin for file reads
	return Promise.resolve(new ArrayBuffer(0));
}

export function getFileStats(path: string): Promise<FileStats> {
	return invoke<FileStats>("file_stat", { filePath: path });
}

export function ensureDir(path: string): Promise<boolean> {
	return invoke<boolean>("file_ensure_dir", { dirPath: path });
}

export function copyToDownloads(filePath: string): Promise<string> {
	return invoke<string>("file_copy_to_downloads", { filePath });
}

// ── Cloud ───────────────────────────────────────────────────────────────────
// API key is stored in the Rust backend — never exposed to the frontend.

export function getCloudConfig(): Promise<CloudConfig> {
	return invoke<CloudConfig>("cloud_get_config");
}

export async function uploadClip(opts: UploadClipOpts): Promise<UploadClipResponse> {
	// Reject files over 200MB
	if (opts.bytes > 200 * 1024 * 1024) {
		throw new Error(`File too large for upload (${Math.round(opts.bytes / 1024 / 1024)}MB). Max 200MB.`);
	}

	// Use native Rust upload — avoids reading 100MB+ files into WebView memory
	// which caused WebView2 crashes under GPU pressure. The Rust side streams the
	// file and emits real "upload:progress" events (see onUploadProgress). We pass
	// the real clip duration when known; Rust falls back to 30s when it's undefined.
	await invoke<string>("native_upload_clip", {
		filePath: opts.filePath,
		durationSeconds: opts.durationSeconds,
	});

	// Return minimal response (the actual stream info isn't needed for the queue)
	return { streamUid: undefined, shareUrl: undefined } as any;
}

/**
 * Real upload progress emitted by the Rust backend during the file transfer.
 * `id` is the clip's file path (matches UploadJob.path). Percent is 0–100.
 */
export function onUploadProgress(
	cb: (p: { id: string; sent: number; total: number; percent: number }) => void,
): Promise<UnlistenFn> {
	return listen<{ id: string; sent: number; total: number; percent: number }>(
		"upload:progress",
		(event) => cb(event.payload),
	);
}

/**
 * Emitted by the Rust backend when a transient upload failure is being retried
 * with exponential backoff. Informational only — the retry happens in Rust.
 */
export function onUploadRetry(
	cb: (p: { id: string; attempt: number; maxAttempts: number; message: string }) => void,
): Promise<UnlistenFn> {
	return listen<{ id: string; attempt: number; maxAttempts: number; message: string }>(
		"upload:retry",
		(event) => cb(event.payload),
	);
}

export async function notifyUploadStatus(body: UploadStatusBody): Promise<void> {
	await invoke("cloud_notify_status", { body }).catch(() => {});
}

// ── MP4 Inspection ──────────────────────────────────────────────────────────
export function inspectMp4(filePath: string): Promise<Mp4Info> {
	return invoke<Mp4Info>('mp4_inspect', { filePath });
}

export function getMp4Keyframes(filePath: string): Promise<number[]> {
	return invoke<number[]>('mp4_keyframes', { filePath });
}

// ── Lossless Trim ───────────────────────────────────────────────────────────
export function losslessTrimClip(
	inputPath: string,
	outputPath: string,
	start: number,
	end: number,
): Promise<TrimResult> {
	return invoke<TrimResult>('lossless_trim_clip', { inputPath, outputPath, start, end });
}

// ── Watch Folder ────────────────────────────────────────────────────────────
export function watchFolderStart(): Promise<boolean> {
	return invoke<boolean>("watch_folder_start");
}

export function watchFolderStop(): Promise<boolean> {
	return invoke<boolean>("watch_folder_stop");
}

export function watchFolderStatus(): Promise<{ active: boolean; filesDetected: number }> {
	return invoke<{ active: boolean; filesDetected: number }>("watch_folder_status");
}

export function onWatchFolderNewFile(
	callback: (file: { path: string; name: string; size: number }) => void,
): Promise<UnlistenFn> {
	return listen<{ path: string; name: string; size: number }>("watch-folder:new-file", (event) => {
		callback(event.payload);
	});
}

// ── Auto-update ─────────────────────────────────────────────────────────────
export interface UpdateInfo {
	available: boolean;
	version: string;
	current_version: string;
	notes: string;
}

/** Check the update endpoint for a newer release (metadata only). */
export function checkForUpdates(): Promise<UpdateInfo> {
	return invoke<UpdateInfo>("check_for_updates");
}

/** Download + install the available update and relaunch. */
export function installUpdate(): Promise<void> {
	return invoke<void>("install_update");
}

/** Progress events (0-100) emitted while an update downloads. */
export function onUpdateProgress(cb: (pct: number) => void): Promise<UnlistenFn> {
	return listen<number>("update:progress", (event) => cb(event.payload));
}

// ── Convenience: unified bridge object ──────────────────────────────────────
const bridge = {
	minimize,
	maximize,
	close,
	getSources,
	getWgcSources,
	setPendingSource,
	setRecordingState,
	saveRecording,
	exportRecording,
	browseSaveExport,
	wgcStartRecording,
	wgcSaveClip,
	wgcStopRecording,
	wgcPauseToggle,
	wgcSaveFullRecording,
	onWgcClipSaved,
	onWgcLog,
	onHotkeyRecord,
	onHotkeyClip30Sec,
	onHotkeyClip1Min,
	onHotkeyClip5Min,
	onPlayClipSound,
	getSettings,
	setSetting,
	setAllSettings,
	suspendHotkeys,
	resumeHotkeys,
	browseFolder,
	browseImportFolder,
	browseFile,
	openFolder,
	openFile,
	showInFolder,
	listAudioDevices,
	getDefaultAudioDevices,
	listClips,
	deleteClip,
	renameClip,
	importClip,
	importFolder,
	getSystemInfo,
	getActiveWindowTitle,
	readFile,
	getFileStats,
	ensureDir,
	copyToDownloads,
	getCloudConfig,
	uploadClip,
	onUploadProgress,
	onUploadRetry,
	notifyUploadStatus,
	inspectMp4,
	getMp4Keyframes,
	losslessTrimClip,
	watchFolderStart,
	watchFolderStop,
	watchFolderStatus,
	onWatchFolderNewFile,
	checkForUpdates,
	installUpdate,
	onUpdateProgress,
};

export default bridge;
