export interface AppSettings {
	outputFolder: string;
	hotkeyClip1Min: string;
	hotkeyClip5Min: string;
	hotkeyRecord: string;
	bufferDuration: number;
	resolution: string;
	fps: number;
	aspectRatio: string;
	encoder: string;
	bitrate: number;
	audioBitrate: number;
	captureAudio: boolean;
	captureMic: boolean;
	audioSource: string;
	audioInputDeviceId: string;
	gameDetect: boolean;
	autoUpload: boolean;
	minimizeToTray: boolean;
	overlayEnabled: boolean;
	cloudEnabled: boolean;
	cloudPairCode: string;
	uploadBandwidth: number;
	deleteAfterUpload: boolean;
	desktopDeviceId: string;
	desktopAudioDeviceId: string;
}

export interface UploadJob {
	id: string;
	path: string;
	name: string;
	size: number;
	progress: number;
	status: "queued" | "uploading" | "done" | "failed";
	error?: string;
	streamUid?: string;
	shareUrl?: string;
	trimStart?: number;
	trimEnd?: number;
	cuts?: { start: number; end: number }[];
	retryCount?: number;
}

export interface UploadClipResponse {
	id: string;
	streamUid: string;
	uploadUrl: string;
	shareUrl: string;
	playerUrl: string;
	streamUrl: string;
	thumbnailUrl: string;
}

export interface ScreenSource {
	id: string;
	name: string;
	thumbnail: string;
	appIcon: string | null;
}

export interface WgcSource {
	id: string;          // e.g. "monitor:12345" or "hwnd:67890"
	name: string;
	source_type: "monitor" | "window";
	width: number;
	height: number;
}

export interface AudioDevice {
	id: string;          // WASAPI persistent device ID
	kind: "input" | "output";
	name: string;        // Friendly device name
}

export interface ClipFile {
	name: string;
	path: string;
	size: number;
	createdAt: string;
}

export type Page = "capture" | "library" | "editor" | "settings";

export interface UploadClipOpts {
	desktopDeviceId: string;
	filePath: string;
	fileName: string;
	durationSeconds: number;
	bytes: number;
	capturedAt: string;
	encoder?: string;
	trimStart?: number;
	trimEnd?: number;
	cuts?: { start: number; end: number }[];
}

export interface UploadStatusBody {
	desktopDeviceId: string;
	desktopName: string;
	queuedCount: number;
	waitingForGameplayCount: number;
	uploadingCount: number;
	uploadedCount: number;
	failedCount: number;
	currentProgressPercent: number;
	currentStatus: string;
}

declare global {
	interface Window {
		clipsta: {
			minimize(): void;
			maximize(): void;
			close(): void;
			getSources(): Promise<ScreenSource[]>;
			getWgcSources(): Promise<WgcSource[]>;
			setPendingSource(id: string | null): Promise<void>;
			setRecordingState(r: boolean): Promise<void>;
			saveRecording(buf: ArrayBuffer, name: string): Promise<string>;
			exportRecording(inputPath: string, outputPath: string, opts: ExportOpts): Promise<string>;
			browseSaveExport(defaultName: string): Promise<string | null>;
			onHotkeyRecord(cb: () => void): void;
			onHotkeyClip1Min(cb: () => void): void;
			onHotkeyClip5Min(cb: () => void): void;
			getSettings(): Promise<AppSettings>;
			setSetting(key: string, value: unknown): Promise<boolean>;
			setAllSettings(s: Partial<AppSettings>): Promise<boolean>;
			browseFolder(): Promise<string | null>;
			browseImportFolder(): Promise<string | null>;
			browseFile(): Promise<string | null>;
			openFolder(p: string): Promise<void>;
			openFile(p: string): Promise<void>;
			showInFolder(p: string): Promise<void>;
			listClips(): Promise<ClipFile[]>;
			deleteClip(p: string): Promise<boolean>;
			renameClip(old: string, name: string): Promise<string>;
			importClip(sourcePath: string): Promise<string>;
			importFolder(folderPath: string): Promise<string[]>;
			getSystemInfo(): Promise<SystemInfo>;
			getCloudConfig(): Promise<CloudConfig>;
			readFile(path: string): Promise<ArrayBuffer>;
			getFileStats(path: string): Promise<{ size: number; modifiedAt: string }>;
			ensureDir(path: string): Promise<boolean>;
			copyToDownloads(filePath: string): Promise<string>;
			uploadClip(opts: UploadClipOpts): Promise<UploadClipResponse>;
			notifyUploadStatus(body: UploadStatusBody): Promise<void>;
			listAudioDevices(): Promise<AudioDevice[]>;
			// WGC recording IPC
			wgcStartRecording(opts: { sourceId: string | null; fps: number; noAudio: boolean; micDevice?: string; loopbackDevice?: string }): Promise<{ width: number; height: number; fps: number } | null>;
			wgcSaveClip(opts: { seconds: number; fileName: string; sourceId: string | null; fps?: number; noAudio?: boolean; micDevice?: string; loopbackDevice?: string }): Promise<string | null>;
			wgcStopRecording(): Promise<void>;
			wgcSaveFullRecording(): Promise<string | null>;
			onWgcClipSaved(cb: (path: string) => void): void;
			onWgcLog(cb: (level: string, line: string) => void): void;
		};
	}
}

export interface FileStats {
	size: number;
	modifiedAt: string;
}

export interface TimelineEntry {
	id: string;
	path: string;
	name: string;
	trimIn: number;
	trimOut: number;
}

export interface ExportOpts {
	format: string;
	aspectRatio: string;
	resolution: string;
	fps?: number;
	encoder?: string;
	trimStart?: number;
	trimEnd?: number;
	cuts?: { start: number; end: number }[];
	timeline?: { path: string; trimIn: number; trimOut: number }[];
}

export interface CloudConfig {
	apiBase: string;
	apiKey: string;
}

export interface SystemInfo {
	platform: string;
	arch: string;
	totalMem: number;
	freeMem: number;
	cpus: number;
}

export {};
