import { contextBridge, ipcRenderer } from "electron";

contextBridge.exposeInMainWorld("clipsta", {
	// Window
	minimize: () => ipcRenderer.send("win:minimize"),
	maximize: () => ipcRenderer.send("win:maximize"),
	close: () => ipcRenderer.send("win:close"),

	// Sources
	getSources: () => ipcRenderer.invoke("sources:get"),

	// Recording
	setPendingSource: (id: string | null) => ipcRenderer.invoke("recording:setPendingSource", id),
	setRecordingState: (r: boolean) => ipcRenderer.invoke("recording:setState", r),
	saveRecording: (buf: ArrayBuffer, name: string) => ipcRenderer.invoke("recording:save", buf, name),
	exportRecording: (inputPath: string, outputPath: string, opts: object) =>
		ipcRenderer.invoke("recording:export", inputPath, outputPath, opts),

	// Hotkey events (main → renderer)
	onHotkeyRecord: (cb: () => void) => ipcRenderer.on("hotkey:record", cb),
	onHotkeyClip1Min: (cb: () => void) => ipcRenderer.on("hotkey:clip1min", cb),
	onHotkeyClip5Min: (cb: () => void) => ipcRenderer.on("hotkey:clip5min", cb),

	// Settings
	getSettings: () => ipcRenderer.invoke("settings:getAll"),
	setSetting: (key: string, value: unknown) => ipcRenderer.invoke("settings:set", key, value),
	setAllSettings: (s: object) => ipcRenderer.invoke("settings:setAll", s),

	// Dialogs
	browseFolder: () => ipcRenderer.invoke("dialog:folder"),
	browseFile: () => ipcRenderer.invoke("dialog:file"),
	browseSaveExport: (defaultName: string) => ipcRenderer.invoke("dialog:saveExport", defaultName),

	// Shell
	openFolder: (p: string) => ipcRenderer.invoke("shell:openFolder", p),
	openFile: (p: string) => ipcRenderer.invoke("shell:openFile", p),
	showInFolder: (p: string) => ipcRenderer.invoke("shell:showItem", p),

	// Clips
	listClips: () => ipcRenderer.invoke("clips:list"),
	deleteClip: (p: string) => ipcRenderer.invoke("clips:delete", p),
	renameClip: (old: string, name: string) => ipcRenderer.invoke("clips:rename", old, name),
	importClip: (sourcePath: string) => ipcRenderer.invoke("clips:import", sourcePath),

	// System
	getSystemInfo: () => ipcRenderer.invoke("system:info"),

	// File
	readFile: (p: string) => ipcRenderer.invoke("file:read", p),

	// Cloud config
	getCloudConfig: () => ipcRenderer.invoke("cloud:getConfig"),

	// Cloud upload
	uploadClip: (opts: object) => ipcRenderer.invoke("upload:clip", opts),
	notifyUploadStatus: (body: object) => ipcRenderer.invoke("upload:status", body),
});
