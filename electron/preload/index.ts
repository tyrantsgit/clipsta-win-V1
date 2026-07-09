import { contextBridge, ipcRenderer } from "electron";

contextBridge.exposeInMainWorld("clipsta", {
	// Window
	minimize: () => ipcRenderer.send("win:minimize"),
	maximize: () => ipcRenderer.send("win:maximize"),
	close: () => ipcRenderer.send("win:close"),

	// Sources (legacy Electron desktopCapturer - still used for thumbnails/icons)
	getSources: () => ipcRenderer.invoke("sources:get"),
	// WGC sources (from clipsta-capture binary)
	getWgcSources: () => ipcRenderer.invoke("wgc:sources"),

	// Legacy recording (kept for compatibility)
	setPendingSource: (id: string | null) => ipcRenderer.invoke("recording:setPendingSource", id),
	setRecordingState: (r: boolean) => ipcRenderer.invoke("recording:setState", r),
	saveRecording: (buf: ArrayBuffer, name: string) => ipcRenderer.invoke("recording:save", buf, name),
	exportRecording: (inputPath: string, outputPath: string, opts: object) =>
		ipcRenderer.invoke("recording:export", inputPath, outputPath, opts),

	// WGC recording IPC
	wgcStartRecording: (opts: object) => ipcRenderer.invoke("wgc:startRecording", opts),
	wgcSaveClip: (opts: { seconds: number; fileName: string; sourceId: string | null; fps?: number; noAudio?: boolean; micDevice?: string; loopbackDevice?: string }) =>
		ipcRenderer.invoke("wgc:saveClip", opts),
	wgcStopRecording: () => ipcRenderer.invoke("wgc:stopRecording"),
	wgcSaveFullRecording: () => ipcRenderer.invoke("wgc:saveFullRecording"),
	onWgcClipSaved: (cb: (path: string) => void) => {
		ipcRenderer.removeAllListeners("wgc:clipSaved");
		ipcRenderer.on("wgc:clipSaved", (_e, path) => cb(path));
	},
	onWgcLog: (cb: (level: string, line: string) => void) => {
		ipcRenderer.removeAllListeners("wgc:log");
		ipcRenderer.on("wgc:log", (_e, { level, line }) => cb(level, line));
	},

	// Hotkey events (main → renderer)
	// Each call replaces the previous listener so they never stack.
	onHotkeyRecord: (cb: () => void) => {
		ipcRenderer.removeAllListeners("hotkey:record");
		ipcRenderer.on("hotkey:record", cb);
	},
	onHotkeyClip1Min: (cb: () => void) => {
		ipcRenderer.removeAllListeners("hotkey:clip1min");
		ipcRenderer.on("hotkey:clip1min", cb);
	},
	onHotkeyClip5Min: (cb: () => void) => {
		ipcRenderer.removeAllListeners("hotkey:clip5min");
		ipcRenderer.on("hotkey:clip5min", cb);
	},

	// Settings
	getSettings: () => ipcRenderer.invoke("settings:getAll"),
	setSetting: (key: string, value: unknown) => ipcRenderer.invoke("settings:set", key, value),
	setAllSettings: (s: object) => ipcRenderer.invoke("settings:setAll", s),

	// Dialogs
	browseFolder: () => ipcRenderer.invoke("dialog:folder"),
	browseImportFolder: () => ipcRenderer.invoke("dialog:importFolder"),
	browseFile: () => ipcRenderer.invoke("dialog:file"),
	browseSaveExport: (defaultName: string) => ipcRenderer.invoke("dialog:saveExport", defaultName),

	// Audio devices (WASAPI enumeration via Rust binary)
	listAudioDevices: () => ipcRenderer.invoke("audio:listDevices"),

	// Shell
	openFolder: (p: string) => ipcRenderer.invoke("shell:openFolder", p),
	openFile: (p: string) => ipcRenderer.invoke("shell:openFile", p),
	showInFolder: (p: string) => ipcRenderer.invoke("shell:showItem", p),

	// Clips
	listClips: () => ipcRenderer.invoke("clips:list"),
	deleteClip: (p: string) => ipcRenderer.invoke("clips:delete", p),
	renameClip: (old: string, name: string) => ipcRenderer.invoke("clips:rename", old, name),
	importClip: (sourcePath: string) => ipcRenderer.invoke("clips:import", sourcePath),
	importFolder: (folderPath: string) => ipcRenderer.invoke("clips:importFolder", folderPath),

	// System
	getSystemInfo: () => ipcRenderer.invoke("system:info"),

	// File
	readFile: (p: string) => ipcRenderer.invoke("file:read", p),
	getFileStats: (p: string) => ipcRenderer.invoke("file:stat", p),
	ensureDir: (p: string) => ipcRenderer.invoke("file:ensureDir", p),
	copyToDownloads: (filePath: string) => ipcRenderer.invoke("file:copyToDownloads", filePath),

	// Cloud config
	getCloudConfig: () => ipcRenderer.invoke("cloud:getConfig"),

	// Cloud upload
	uploadClip: (opts: object) => ipcRenderer.invoke("upload:clip", opts),
	notifyUploadStatus: (body: object) => ipcRenderer.invoke("upload:status", body),
});
