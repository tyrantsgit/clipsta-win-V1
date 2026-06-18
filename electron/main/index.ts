import {
	app,
	BrowserWindow,
	ipcMain,
	desktopCapturer,
	globalShortcut,
	dialog,
	shell,
	Tray,
	Menu,
	nativeImage,
	powerSaveBlocker,
	session,
} from "electron";
import path from "path";
import fs from "fs";
import { execFile } from "child_process";
import Store from "electron-store";
import os from "os";

// ── Persistent settings ───────────────────────────────────────────────────────
	const store = new Store<AppSettings>({
		defaults: {
			outputFolder: "",
		hotkeyClip1Min: "Alt+F9",
		hotkeyClip5Min: "Alt+F10",
		hotkeyRecord: "F9",
		bufferDuration: 60,
		resolution: "1080p",
		fps: 60,
		aspectRatio: "16:9",
		encoder: "auto",
		bitrate: 8000,
		audioBitrate: 128,
		captureAudio: true,
		captureMic: false,
		audioSource: "desktop",
		audioInputDeviceId: "",
		gameDetect: true,
		autoUpload: false,
		minimizeToTray: true,
		overlayEnabled: true,
		cloudEnabled: false,
		cloudPairCode: "",
		uploadBandwidth: 0,
		deleteAfterUpload: false,
		desktopDeviceId: "",
	},
});

interface AppSettings {
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

let mainWindow: BrowserWindow | null = null;
let tray: Tray | null = null;
let isRecording = false;
let powerBlockId: number | null = null;
let closeFromUi = false;
let pendingSourceId: string | null = null;

// ── Single-instance lock ──────────────────────────────────────────────────────
const gotLock = app.requestSingleInstanceLock();
if (!gotLock) app.quit();

app.on("second-instance", () => {
	if (mainWindow) {
		if (mainWindow.isMinimized()) mainWindow.restore();
		mainWindow.show();
		mainWindow.focus();
	}
});

// ── App lifecycle ─────────────────────────────────────────────────────────────
app.whenReady().then(async () => {
	// Allow media access
	session.defaultSession.setPermissionRequestHandler((_wc, permission, callback) => {
		if (permission === "media" || permission === "display-capture") return callback(true);
		callback(false);
	});

	// Handle programmatic getDisplayMedia — return the source the renderer chose
	session.defaultSession.setDisplayMediaRequestHandler(async (_request, callback) => {
		const sources = await desktopCapturer.getSources({
			types: ["screen", "window"],
			thumbnailSize: { width: 320, height: 180 },
		});
		const source = pendingSourceId
			? sources.find((s) => s.id === pendingSourceId)
			: null;
		if (source) {
			callback({ video: source, audio: "loopback" });
		} else {
			// fallback: no pending source, let native picker show
			callback({});
		}
	});

	ensureOutputFolder();
	ensureDesktopDeviceId();
	await createWindow();
	createTray();
	registerAllHotkeys();
});

app.on("window-all-closed", () => { /* stay in tray */ });
app.on("will-quit", () => { globalShortcut.unregisterAll(); });
app.on("activate", () => { if (!mainWindow) createWindow(); });

// ── Window ────────────────────────────────────────────────────────────────────
async function createWindow() {
	mainWindow = new BrowserWindow({
		width: 1400,
		height: 900,
		minWidth: 1100,
		minHeight: 720,
		frame: false,
		transparent: false,
		backgroundColor: "#0a0a0a",
		icon: path.join(__dirname, "../../dist/icon.ico"),
		webPreferences: {
			preload: path.join(__dirname, "../preload/index.js"),
			contextIsolation: true,
			nodeIntegration: false,
			webSecurity: false, // allow blob: URLs for video preview
		},
		show: false,
	});

	mainWindow.once("ready-to-show", () => {
		mainWindow!.show();
		mainWindow!.focus();
	});

	mainWindow.on("close", (e) => {
		if (closeFromUi) {
			closeFromUi = false;
			if (store.get("minimizeToTray")) {
				e.preventDefault();
				mainWindow!.hide();
				return;
			}
			// User clicked X with minimizeToTray off — let window close naturally
			return;
		}
		// System close (installer, OS shutdown) — exit immediately
		globalShortcut.unregisterAll();
		app.exit(0);
	});

	const devUrl = process.env["VITE_DEV_SERVER_URL"];
	if (devUrl) {
		await mainWindow.loadURL(devUrl);
	} else {
		await mainWindow.loadFile(path.join(__dirname, "../../dist/index.html"));
	}
}

// ── Tray ──────────────────────────────────────────────────────────────────────
function createTray() {
	const icoPath = path.join(__dirname, "../../dist/icon.ico");
	const icon = fs.existsSync(icoPath)
		? nativeImage.createFromPath(icoPath).resize({ width: 16, height: 16 })
		: nativeImage.createEmpty();
	tray = new Tray(icon);
	tray.setToolTip("Clipsta");
	refreshTrayMenu();
	tray.on("double-click", showWindow);
}

function refreshTrayMenu() {
	const menu = Menu.buildFromTemplate([
		{ label: "Open Clipsta", click: showWindow },
		{ type: "separator" },
		{
			label: isRecording ? "⏹  Stop Recording" : "⏺  Start Recording",
			click: () => mainWindow?.webContents.send("hotkey:record"),
		},
		{ label: "Save 1-min Clip", click: () => mainWindow?.webContents.send("hotkey:clip1min") },
		{ label: "Save 5-min Clip", click: () => mainWindow?.webContents.send("hotkey:clip5min") },
		{ type: "separator" },
		{ label: "Open Clips Folder", click: () => shell.openPath(store.get("outputFolder")) },
		{ type: "separator" },
		{ label: "Quit Clipsta", click: () => { globalShortcut.unregisterAll(); app.exit(0); } },
	]);
	tray?.setContextMenu(menu);
}

function showWindow() {
	if (!mainWindow) createWindow();
	else { mainWindow.show(); mainWindow.focus(); }
}

// ── Hotkeys ───────────────────────────────────────────────────────────────────
function registerAllHotkeys() {
	globalShortcut.unregisterAll();
	const s = store.store;

	tryRegister(s.hotkeyRecord, () => mainWindow?.webContents.send("hotkey:record"));
	tryRegister(s.hotkeyClip1Min, () => mainWindow?.webContents.send("hotkey:clip1min"));
	tryRegister(s.hotkeyClip5Min, () => mainWindow?.webContents.send("hotkey:clip5min"));
}

function tryRegister(accelerator: string, fn: () => void) {
	try {
		if (accelerator) globalShortcut.register(accelerator, fn);
	} catch (e) {
		console.warn("Could not register hotkey:", accelerator, e);
	}
}

// ── Output folder ─────────────────────────────────────────────────────────────
function ensureOutputFolder() {
	let f = store.get("outputFolder");
	if (!f) {
		f = path.join(app.getPath("videos"), "Clipsta");
		store.set("outputFolder", f);
	}
	if (!fs.existsSync(f)) fs.mkdirSync(f, { recursive: true });
}

// ── Desktop device ID ─────────────────────────────────────────────────────────
function ensureDesktopDeviceId() {
	let id = store.get("desktopDeviceId");
	if (!id) {
		id = `desktop_${crypto.randomUUID().replaceAll("-", "")}`;
		store.set("desktopDeviceId", id);
	}
}

// ── IPC: window controls ──────────────────────────────────────────────────────
ipcMain.on("win:minimize", () => mainWindow?.minimize());
ipcMain.on("win:maximize", () =>
	mainWindow?.isMaximized() ? mainWindow.unmaximize() : mainWindow?.maximize()
);
ipcMain.on("win:close", () => {
	closeFromUi = true;
	if (store.get("minimizeToTray")) mainWindow?.hide();
	else mainWindow?.close();
});

// ── IPC: screen sources ───────────────────────────────────────────────────────
ipcMain.handle("sources:get", async () => {
	const sources = await desktopCapturer.getSources({
		types: ["window", "screen"],
		thumbnailSize: { width: 320, height: 180 },
		fetchWindowIcons: true,
	});
	return sources.map((s) => ({
		id: s.id,
		name: s.name,
		thumbnail: s.thumbnail.toDataURL(),
		appIcon: s.appIcon?.toDataURL() ?? null,
	}));
});

// ── IPC: pending source ID for getDisplayMedia ────────────────────────────────
ipcMain.handle("recording:setPendingSource", (_e, id: string | null) => {
	pendingSourceId = id;
});

// ── IPC: recording state ──────────────────────────────────────────────────────
ipcMain.handle("recording:setState", (_e, recording: boolean) => {
	isRecording = recording;
	refreshTrayMenu();
	if (recording) {
		powerBlockId = powerSaveBlocker.start("prevent-display-sleep");
	} else if (powerBlockId !== null) {
		powerSaveBlocker.stop(powerBlockId);
		powerBlockId = null;
	}
});

// ── IPC: save blob from renderer ──────────────────────────────────────────────
ipcMain.handle("dialog:saveExport", async (_e, defaultName: string) => {
	const folder = store.get("outputFolder") || app.getPath("videos");
	const r = await dialog.showSaveDialog(mainWindow!, {
		title: "Export clip",
		defaultPath: path.join(folder, defaultName),
		filters: [
			{ name: "MP4 Video", extensions: ["mp4"] },
			{ name: "WebM Video", extensions: ["webm"] },
			{ name: "MKV Video", extensions: ["mkv"] },
			{ name: "MOV Video", extensions: ["mov"] },
			{ name: "All Files", extensions: ["*"] },
		],
	});
	return r.canceled ? null : r.filePath;
});

ipcMain.handle("recording:save", async (_e, arrayBuffer: ArrayBuffer, fileName: string) => {
	const folder = store.get("outputFolder");
	ensureOutputFolder();
	const filePath = path.join(folder, fileName);
	fs.writeFileSync(filePath, Buffer.from(arrayBuffer));
	return filePath;
});

// ── IPC: export via ffmpeg ────────────────────────────────────────────────────
ipcMain.handle(
	"recording:export",
	async (_e, inputPath: string, outputPath: string, opts: ExportOpts): Promise<string> => {
		const vfFilters: string[] = [];

		// Cuts — build select expression to skip removed segments
		if (opts.cuts && opts.cuts.length > 0) {
			const terms = opts.cuts
				.filter((c) => c.start < c.end)
				.map((c) => `between(t,${c.start},${c.end})`);
			if (terms.length) {
				vfFilters.push(`select='not(${terms.join("+")})',setpts=N/FRAME_RATE/TB`);
			}
		}

		// Aspect ratio crop (before scale — clamps to input width so it's safe on already-cropped sources)
		if (opts.aspectRatio === "9:16") vfFilters.push("crop=min(iw\\,ih*9/16):ih");
		else if (opts.aspectRatio === "4:3") vfFilters.push("crop=min(iw\\,ih*4/3):ih");

		// Resolution scale (auto-calculate the other dimension to preserve aspect)
		const pValues: Record<string, number> = {
			"480p": 480, "720p": 720, "1080p": 1080, "1440p": 1440, "4k": 2160,
		};
		const p = opts.resolution ? pValues[opts.resolution] : null;
		if (p) {
			if (opts.aspectRatio !== "9:16") {
				vfFilters.push(`scale=-2:${p}`);
			} else {
				vfFilters.push(`scale=${p}:-2`);
			}
		}

		const args: string[] = [];
		let isMultiClip = false;

		// Multi-clip timeline: use concat filter (handles varying codecs, applies trim per-clip)
		if (opts.timeline && opts.timeline.length > 1) {
			isMultiClip = true;
			const filterParts: string[] = [];
			const inputLabels: string[] = [];
			for (let i = 0; i < opts.timeline.length; i++) {
				const clip = opts.timeline[i];
				const p = clip.path.replace(/\\/g, "/");
				args.push("-hwaccel", "auto", "-i", p);
				const vlabel = `v${i}`;
				const alabel = `a${i}`;
				if (clip.trimIn > 0 || (clip.trimOut > 0 && clip.trimOut > clip.trimIn)) {
					const trimStart = clip.trimIn > 0 ? `start=${clip.trimIn}` : "";
					const trimEnd = clip.trimOut > 0 ? `end=${clip.trimOut}` : "";
					const trim = [trimStart, trimEnd].filter(Boolean).join(":");
					filterParts.push(`[${i}:v]trim=${trim},setpts=PTS-STARTPTS[${vlabel}]`);
					filterParts.push(`[${i}:a]atrim=${trim},asetpts=PTS-STARTPTS[${alabel}]`);
				} else {
					filterParts.push(`[${i}:v]setpts=PTS-STARTPTS[${vlabel}]`);
					filterParts.push(`[${i}:a]asetpts=PTS-STARTPTS[${alabel}]`);
				}
				inputLabels.push(`[${vlabel}][${alabel}]`);
			}
			const concatStr = `${inputLabels.join("")}concat=n=${opts.timeline.length}:v=1:a=1:unsafe=1[vmerge][amerge]`;
			filterParts.push(concatStr);

			// Append resolution/crop filters to the merged video
			if (vfFilters.length) {
				filterParts.push(`[vmerge]${vfFilters.join(",")}[vout]`);
				args.push("-filter_complex", filterParts.join(";"));
				args.push("-map", "[vout]", "-map", "[amerge]");
			} else {
				args.push("-filter_complex", filterParts.join(";"));
				args.push("-map", "[vmerge]", "-map", "[amerge]");
			}
		} else {
			// Single clip
			args.push("-hwaccel", "auto", "-i", inputPath);
			if (opts.trimStart != null) args.push("-ss", String(opts.trimStart));
			if (opts.trimEnd != null) args.push("-to", String(opts.trimEnd));
		}

		if (!isMultiClip && vfFilters.length) args.push("-vf", vfFilters.join(","));
		if (opts.cuts && opts.cuts.length > 0) {
			const aTerms = opts.cuts
				.filter((c) => c.start < c.end)
				.map((c) => `between(t,${c.start},${c.end})`);
			if (aTerms.length) {
				args.push("-af", `aselect='not(${aTerms.join("+")})',asetpts=N/SR/TB`);
			}
		}

		args.push("-c:v", "libx264", "-preset", "ultrafast", "-crf", "23");
		if (opts.fps) args.push("-r", String(opts.fps));
		args.push("-c:a", "aac", "-b:a", "192k");
		args.push("-movflags", "+faststart", "-y", outputPath);

		return new Promise((resolve, reject) => {
			execFile("ffmpeg", args, { timeout: 300000 }, (err2, _stdout, stderr) => {
				if (err2) {
					const detail = stderr ? stderr.split("\n").slice(-3).join(" ").trim() : err2.message;
					reject(`FFmpeg error: ${detail}`);
				} else {
					resolve(outputPath);
				}
			});
		});
	}
);

// ── IPC: settings ─────────────────────────────────────────────────────────────
ipcMain.handle("settings:getAll", () => store.store);
ipcMain.handle("settings:set", (_e, key: keyof AppSettings, value: unknown) => {
	(store as any).set(key, value);
	// Re-register hotkeys if any changed
	if (["hotkeyRecord", "hotkeyClip1Min", "hotkeyClip5Min"].includes(key))
		registerAllHotkeys();
	if (key === "outputFolder") ensureOutputFolder();
	return true;
});
ipcMain.handle("settings:setAll", (_e, settings: Partial<AppSettings>) => {
	Object.entries(settings).forEach(([k, v]) => (store as any).set(k, v));
	registerAllHotkeys();
	ensureOutputFolder();
	return true;
});

// ── IPC: browse folder ────────────────────────────────────────────────────────
ipcMain.handle("dialog:folder", async () => {
	const r = await dialog.showOpenDialog(mainWindow!, {
		properties: ["openDirectory", "createDirectory"],
		title: "Choose clips output folder",
	});
	return r.canceled ? null : r.filePaths[0];
});

ipcMain.handle("dialog:file", async () => {
	const r = await dialog.showOpenDialog(mainWindow!, {
		properties: ["openFile"],
		title: "Open video file",
		filters: [
			{ name: "Video Files", extensions: ["webm", "mp4", "mkv", "mov", "avi"] },
			{ name: "All Files", extensions: ["*"] },
		],
	});
	return r.canceled ? null : r.filePaths[0];
});

// ── IPC: open paths ───────────────────────────────────────────────────────────
ipcMain.handle("shell:openFolder", (_e, p: string) => shell.openPath(p));
ipcMain.handle("shell:openFile", (_e, p: string) => shell.openPath(p));
ipcMain.handle("shell:showItem", (_e, p: string) => shell.showItemInFolder(p));

// ── IPC: clip library ─────────────────────────────────────────────────────────
ipcMain.handle("clips:list", () => {
	const folder = store.get("outputFolder");
	if (!fs.existsSync(folder)) return [];
	return fs
		.readdirSync(folder)
		.filter((f) => /\.(webm|mp4|mkv|mov)$/i.test(f))
		.map((f) => {
			const full = path.join(folder, f);
			const stat = fs.statSync(full);
			return { name: f, path: full, size: stat.size, createdAt: stat.birthtime.toISOString() };
		})
		.sort((a, b) => +new Date(b.createdAt) - +new Date(a.createdAt));
});

ipcMain.handle("clips:delete", (_e, p: string) => {
	if (fs.existsSync(p)) fs.unlinkSync(p);
	return true;
});

ipcMain.handle("clips:rename", (_e, oldPath: string, newName: string) => {
	const dir = path.dirname(oldPath);
	const newPath = path.join(dir, newName);
	fs.renameSync(oldPath, newPath);
	return newPath;
});

ipcMain.handle("clips:import", async (_e, sourcePath: string) => {
	const folder = store.get("outputFolder");
	ensureOutputFolder();
	const name = path.basename(sourcePath);
	const dest = path.join(folder, name);
	if (fs.existsSync(dest)) {
		const ext = path.extname(name);
		const base = path.basename(name, ext);
		let i = 1;
		while (fs.existsSync(dest.replace(name, `${base} (${i})${ext}`))) i++;
		fs.copyFileSync(sourcePath, path.join(folder, `${base} (${i})${ext}`));
		return path.join(folder, `${base} (${i})${ext}`);
	}
	fs.copyFileSync(sourcePath, dest);
	return dest;
});

// ── IPC: system info ──────────────────────────────────────────────────────────
ipcMain.handle("system:info", () => ({
	platform: process.platform,
	arch: process.arch,
	totalMem: os.totalmem(),
	freeMem: os.freemem(),
	cpus: os.cpus().length,
}));

// ── IPC: read file for upload ─────────────────────────────────────────────────
ipcMain.handle("file:read", (_e, filePath: string) => {
	return fs.readFileSync(filePath).buffer;
});

ipcMain.handle("file:stat", (_e, filePath: string) => {
	const s = fs.statSync(filePath);
	return { size: s.size, modifiedAt: s.mtime.toISOString() };
});

ipcMain.handle("file:ensureDir", (_e, dirPath: string) => {
	if (!fs.existsSync(dirPath)) {
		fs.mkdirSync(dirPath, { recursive: true });
	}
	return true;
});

// ── IPC: upload clip to cloud ──────────────────────────────────────────────────
const API_BASE = "https://clipsta-api.godson594.workers.dev";
const DESKTOP_TEST_KEY = process.env.CLIPSTA_API_KEY || "32b28eac803a1b24c19e20665919eaeb7f1493d2b5e3f68be7944db6d9f01b96";

// Expose cloud config to renderer
ipcMain.handle("cloud:getConfig", () => ({
	apiBase: API_BASE,
	apiKey: DESKTOP_TEST_KEY,
}));

ipcMain.handle("upload:clip", async (_e, opts: {
	desktopDeviceId: string;
	filePath: string;
	fileName: string;
	durationSeconds: number;
	bytes: number;
	capturedAt: string;
	trimStart?: number;
	trimEnd?: number;
	cuts?: { start: number; end: number }[];
}) => {
	const controller = new AbortController();
	const uploadTimer = setTimeout(() => controller.abort(), 120000);
	let cleanupPath: string | null = null;

	try {
		// Step 0: If trim/cuts provided, export the edited portion to a temp file
		let uploadPath = opts.filePath;
		let uploadName = opts.fileName;
		if (opts.trimStart != null || opts.trimEnd != null || (opts.cuts && opts.cuts.length > 0)) {
			const ext = path.extname(opts.fileName) || ".mp4";
			const base = path.basename(opts.fileName, ext);
			const tempDir = app.getPath("temp");
			cleanupPath = path.join(tempDir, `clipsta_upload_${base}_${Date.now()}${ext}`);
			const vfFilters: string[] = [];

			if (opts.cuts && opts.cuts.length > 0) {
				const terms = opts.cuts
					.filter((c) => c.start < c.end)
					.map((c) => `between(t,${c.start},${c.end})`);
				if (terms.length) {
					vfFilters.push(`select='not(${terms.join("+")})',setpts=N/FRAME_RATE/TB`);
				}
			}

			const exportArgs: string[] = [
				"-hwaccel", "auto", "-i", opts.filePath,
			];
			if (opts.trimStart != null) exportArgs.push("-ss", String(opts.trimStart));
			if (opts.trimEnd != null) exportArgs.push("-to", String(opts.trimEnd));
			if (vfFilters.length) exportArgs.push("-vf", vfFilters.join(","));
			if (opts.cuts && opts.cuts.length > 0) {
				const aTerms = opts.cuts
					.filter((c) => c.start < c.end)
					.map((c) => `between(t,${c.start},${c.end})`);
				if (aTerms.length) {
					exportArgs.push("-af", `aselect='not(${aTerms.join("+")})',asetpts=N/SR/TB`);
				}
			}
			exportArgs.push("-c:v", "libx264", "-preset", "ultrafast", "-crf", "23");
			exportArgs.push("-c:a", "aac", "-b:a", "192k");
			exportArgs.push("-movflags", "+faststart", "-y", cleanupPath);

			await new Promise<void>((resolve, reject) => {
				execFile("ffmpeg", exportArgs, { timeout: 300000 }, (err2, _stdout, stderr) => {
					if (err2) {
						const detail = stderr ? stderr.split("\n").slice(-3).join(" ").trim() : err2.message;
						reject(`FFmpeg export error: ${detail}`);
					} else {
						resolve();
					}
				});
			});

			uploadPath = cleanupPath;
			uploadName = `${base}.mp4`;
		}

		// Read true file size from disk
		let actualBytes = opts.bytes;
		try {
			const s = fs.statSync(uploadPath);
			actualBytes = s.size;
		} catch { /* fall back */ }

		// Step 1: Request upload URL
		const clipRes = await fetch(`${API_BASE}/clip-uploads`, {
			method: "POST",
			headers: {
				"Content-Type": "application/json",
				"X-Clipsta-Test-Key": DESKTOP_TEST_KEY,
			},
			body: JSON.stringify({
				desktopDeviceId: opts.desktopDeviceId,
				fileName: uploadName,
				durationSeconds: opts.durationSeconds,
				bytes: actualBytes,
				capturedAt: opts.capturedAt,
			}),
			signal: controller.signal,
		});
		if (!clipRes.ok) {
			const errBody = await clipRes.text().catch(() => "");
			throw new Error(`clip-uploads failed: HTTP ${clipRes.status} ${errBody}`);
		}
		const clipData = await clipRes.json();

		// Step 2: Upload file to the returned uploadUrl via multipart form-data
		const fileBuf = fs.readFileSync(uploadPath);
		const mime = uploadName.endsWith(".webm") ? "video/webm"
			: uploadName.endsWith(".mkv") ? "video/x-matroska"
			: uploadName.endsWith(".mov") ? "video/quicktime"
			: "video/mp4";
		const boundary = `----ClipstaBoundary${Math.random().toString(36).slice(2)}`;
		const head = Buffer.from(
			`--${boundary}\r\nContent-Disposition: form-data; name="file"; filename="${uploadName}"\r\nContent-Type: ${mime}\r\n\r\n`,
			"utf-8"
		);
		const tail = Buffer.from(`\r\n--${boundary}--\r\n`, "utf-8");
		const multipartBody = Buffer.concat([head, fileBuf, tail]);

		const uploadRes = await fetch((clipData as any).uploadUrl, {
			method: "POST",
			headers: { "Content-Type": `multipart/form-data; boundary=${boundary}` },
			body: multipartBody,
			signal: controller.signal,
		});
		if (!uploadRes.ok) {
			const errBody = await uploadRes.text().catch(() => "");
			throw new Error(`file upload failed: HTTP ${uploadRes.status} ${errBody}`);
		}

		return clipData;
	} finally {
		clearTimeout(uploadTimer);
		if (cleanupPath) {
			try { fs.unlinkSync(cleanupPath); } catch { /* ignore */ }
		}
	}
});

// ── IPC: notify desktop upload status ──────────────────────────────────────────
ipcMain.handle("upload:status", async (_e, body: {
	desktopDeviceId: string;
	desktopName: string;
	queuedCount: number;
	waitingForGameplayCount: number;
	uploadingCount: number;
	uploadedCount: number;
	failedCount: number;
	currentProgressPercent: number;
	currentStatus: string;
}) => {
	const res = await fetch(`${API_BASE}/desktop-upload-status`, {
		method: "POST",
		headers: {
			"Content-Type": "application/json",
			"X-Clipsta-Test-Key": DESKTOP_TEST_KEY,
		},
		body: JSON.stringify(body),
	});
	if (!res.ok) {
		const errBody = await res.text().catch(() => "");
		console.warn("upload-status failed:", res.status, errBody);
	}
});

interface ExportOpts {
	format: string;
	aspectRatio: string;
	resolution: string;
	trimStart?: number;
	trimEnd?: number;
	cuts?: { start: number; end: number }[];
	timeline?: { path: string; trimIn: number; trimOut: number }[];
}
