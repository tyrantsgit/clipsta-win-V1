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
			browseFile(): Promise<string | null>;
			openFolder(p: string): Promise<void>;
			openFile(p: string): Promise<void>;
			showInFolder(p: string): Promise<void>;
			listClips(): Promise<ClipFile[]>;
			deleteClip(p: string): Promise<boolean>;
			renameClip(old: string, name: string): Promise<string>;
			importClip(sourcePath: string): Promise<string>;
			getSystemInfo(): Promise<SystemInfo>;
			getCloudConfig(): Promise<CloudConfig>;
			readFile(path: string): Promise<ArrayBuffer>;
			getFileStats(path: string): Promise<{ size: number; modifiedAt: string }>;
			ensureDir(path: string): Promise<boolean>;
			uploadClip(opts: UploadClipOpts): Promise<UploadClipResponse>;
			notifyUploadStatus(body: UploadStatusBody): Promise<void>;
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
