export interface AppSettings {
	outputFolder: string;
	hotkeyClip30Sec: string;
	hotkeyClip1Min: string;
	hotkeyClip5Min: string;
	hotkeyRecord: string;
	bufferDuration: number;
	resolution: string;
	fps: number;
	aspectRatio: string;
	encoder: string;
	quality: string;
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
	clipSoundEnabled: boolean;
	cloudEnabled: boolean;
	cloudPairCode: string;
	uploadBandwidth: number;
	deleteAfterUpload: boolean;
	desktopDeviceId: string;
	desktopAudioDeviceId: string;
	watchFolderPath: string;
	watchFolderEnabled: boolean;
	theme: "dark" | "oled";
	multiTrackAudio: boolean;
	startAtLogin: boolean;
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
	id: string;
	name: string;
	source_type: "monitor" | "window";
	width: number;
	height: number;
}

export interface AudioDevice {
	id: string;
	kind: "input" | "output";
	name: string;
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
	brightness?: number;
	contrast?: number;
	saturation?: number;
	speedSegments?: { start: number; end: number; speed: number }[];
	transitions?: { time: number; type: string; duration: number }[];
}

export interface CloudConfig {
	apiBase: string;
}

export interface SystemInfo {
	platform: string;
	arch: string;
	totalMem: number;
	freeMem: number;
	cpus: number;
}

export interface WgcStartOpts {
	sourceId: string | null;
	fps: number;
	noAudio: boolean;
	micDevice?: string;
	loopbackDevice?: string;
}

export interface WgcStartResult {
	width: number;
	height: number;
	fps: number;
}

export interface WgcSaveClipOpts {
	seconds: number;
	fileName: string;
	sourceId: string | null;
	fps?: number;
	noAudio?: boolean;
	micDevice?: string;
	loopbackDevice?: string;
}


export interface Mp4Info {
	duration: number;
	fps: number;
	width: number;
	height: number;
	videoCodec: string;
	audioCodec: string;
	bitrate: number;
	hasAudio: boolean;
	keyframes: number[];
}

export interface TrimResult {
	outputPath: string;
	requestedStart: number;
	actualStart: number;
	requestedEnd: number;
	actualEnd: number;
	duration: number;
	extraBefore: number;
}


// ── Speed Ramping & Transitions ─────────────────────────────────────────────
export interface SpeedSegment {
	id: string;
	start: number;
	end: number;
	speed: number; // 0.25 = quarter speed, 1 = normal, 2 = double, etc.
}

export type TransitionType = "crossfade" | "glitch" | "whip-pan" | "flash" | "zoom-in" | "zoom-out";

export interface Transition {
	id: string;
	time: number; // position in timeline where transition occurs (at a cut point)
	type: TransitionType;
	duration: number; // seconds (0.3 - 1.0 typical)
}
