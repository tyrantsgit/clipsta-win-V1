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
import { execFile, spawn, ChildProcess } from "child_process";
import Store from "electron-store";
import os from "os";

// ── Global error guards — catch any unhandled throw/rejection in main process ──
process.on("uncaughtException", (err) => {
	console.error("[main] uncaughtException:", err?.message ?? err);
});
process.on("unhandledRejection", (reason) => {
	console.error("[main] unhandledRejection:", reason);
});

// ── FFmpeg resolver (bundled resource, then PATH) ──────────────────────────────
let _ffmpegPath: string | null = null;

function getFfmpegPath(): string {
	if (_ffmpegPath) return _ffmpegPath;
	const bundled = path.join(process.resourcesPath, "ffmpeg.exe");
	if (fs.existsSync(bundled)) { _ffmpegPath = bundled; return bundled; }
	const devPath = path.join(__dirname, "../../bin/ffmpeg.exe");
	if (fs.existsSync(devPath)) { _ffmpegPath = devPath; return devPath; }
	_ffmpegPath = "ffmpeg";
	return "ffmpeg";
}

// ── Encoder capability probe (run once at startup) ───────────────────────────
interface EncoderProbeResult {
	nvenc: boolean;
	amf: boolean;
	qsv: boolean;
}
let _encoderProbe: EncoderProbeResult | null = null;
let _probePromise: Promise<EncoderProbeResult> | null = null;

async function probeEncoders(): Promise<EncoderProbeResult> {
	if (_encoderProbe) return _encoderProbe;
	if (_probePromise) return _probePromise;

	const ffmpeg = getFfmpegPath();
	_probePromise = new Promise((resolve) => {
		const proc = spawn(ffmpeg, ["-hide_banner", "-encoders"], { stdio: ["ignore", "pipe", "ignore"] });
		let out = "";
		proc.stdout?.on("data", (d: Buffer) => { out += d.toString(); });
		proc.on("error", () => {
			_encoderProbe = { nvenc: false, amf: false, qsv: false };
			_probePromise = null;
			resolve(_encoderProbe!);
		});
		proc.on("close", () => {
			const result: EncoderProbeResult = {
				nvenc: /VFS?\.\s.*nvenc/.test(out),
				amf:   /VFS?\.\s.*amf/.test(out),
				qsv:   /VFS?\.\s.*qsv/.test(out),
			};
			console.log("[encoder-probe]", result);
			_encoderProbe = result;
			_probePromise = null;
			resolve(result);
		});
		setTimeout(() => {
			if (_probePromise) {
				_encoderProbe = { nvenc: false, amf: false, qsv: false };
				_probePromise = null;
				resolve(_encoderProbe);
			}
		}, 5000);
	});
	return _probePromise;
}

/**
 * Resolve the best available encoder for the requested setting.
 * Returns ffmpeg codec name + extra args.
 * For "auto", picks the best available hardware encoder, falling back to libx264.
 */
function getEncoderArgs(encoder?: string, probe?: EncoderProbeResult): { codec: string; extra: string[] } {
	const p = probe ?? _encoderProbe;
	console.log("[getEncoderArgs]", { encoder, hasProbe: !!p, nvenc: p?.nvenc, amf: p?.amf, qsv: p?.qsv });

	// "auto" — always try NVENC directly. The probe has proven unreliable
	// for HW detection on some systems (false negatives on available NVENC).
	// Users who need software encoding can select "x264 (Software)" in Settings.
	if (!encoder || encoder === "auto") {
		if (p?.nvenc) return nvencArgs();
		console.warn("[getEncoderArgs] probe says NVENC unavailable, trying NVENC directly anyway");
		return nvencArgs();
	}

	switch (encoder) {
		case "NVENC (NVIDIA)":  return nvencArgs();
		case "AMF (AMD)":       return amfArgs();
		case "QuickSync (Intel)": return qsvArgs();
		case "HEVC (H.265)":    return { codec: "libx265", extra: ["-preset", "ultrafast", "-crf", "23"] };
		case "x264 (Software)": return sw264Args();
		default:
			console.warn("[getEncoderArgs] unknown encoder setting:", encoder);
			return sw264Args();
	}
}

function nvencArgs(): { codec: string; extra: string[] } {
	const bitrate = store.get("bitrate") || 20000;
	return {
		codec: "h264_nvenc",
		extra: [
			"-preset",    "p2",
			"-tune",      "hq",
			"-rc",        "vbr",
			"-b:v",       `${bitrate}k`,
			"-maxrate",   `${Math.round(bitrate * 1.5)}k`,
			"-bufsize",   `${bitrate * 2}k`,
			"-pix_fmt",   "yuv420p",
			"-g",         "120",
			"-bf",        "2",
		],
	};
}

function amfArgs(): { codec: string; extra: string[] } {
	return {
		codec: "h264_amf",
		extra: ["-quality", "balanced", "-rc", "cqp", "-qp_i", "22", "-qp_p", "24", "-pix_fmt", "yuv420p"],
	};
}

function qsvArgs(): { codec: string; extra: string[] } {
	return {
		codec: "h264_qsv",
		extra: ["-preset", "veryfast", "-global_quality", "23", "-look_ahead", "1", "-pix_fmt", "yuv420p"],
	};
}

function sw264Args(): { codec: string; extra: string[] } {
	const bitrate = store.get("bitrate") || 20000;
	return {
		codec: "libx264",
		extra: ["-preset", "ultrafast", "-b:v", `${bitrate}k`, "-maxrate", `${Math.round(bitrate * 1.5)}k`, "-bufsize", `${bitrate * 2}k`, "-pix_fmt", "yuv420p"],
	};
}

// ── Persistent settings ───────────────────────────────────────────────────────
	const store = new Store<AppSettings>({
		defaults: {
			outputFolder: "",
		hotkeyClip1Min: "Alt+F9",
		hotkeyClip5Min: "Alt+F10",
		hotkeyRecord: "F9",
		bufferDuration: 300,
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
		cloudEnabled: false,
		cloudPairCode: "",
		uploadBandwidth: 0,
		deleteAfterUpload: false,
		desktopDeviceId: "",
		desktopAudioDeviceId: "",
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
	desktopAudioDeviceId: string;
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

// Prevent GPU process crash limit from killing the app on alt-tab / GPU context loss
app.commandLine.appendSwitch("disable-gpu-process-crash-limit");

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

	// Probe encoder capabilities (NVENC / AMF / QSV) before any recording
	probeEncoders().catch((e) => console.warn("[encoder-probe] failed:", e));

	ensureOutputFolder();
	ensureDesktopDeviceId();
	// Clean up orphaned temp files from previous sessions
	try {
		const tempDir = app.getPath("temp");
		const leftovers = fs.readdirSync(tempDir).filter(f => f.startsWith("clipsta_"));
		for (const f of leftovers) {
			try { fs.unlinkSync(path.join(tempDir, f)); } catch {}
		}
		if (leftovers.length > 0) console.log(`[startup] cleaned ${leftovers.length} orphaned temp files`);
	} catch {}
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
			allowFileAccess: true, // allow file:// URLs for video playback & thumbnails
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

	// Handle GPU / render process crashes — reload instead of hanging
	mainWindow.webContents.on("render-process-gone", (_event, details) => {
		console.error("Render process gone:", details.reason);
		mainWindow?.webContents.reload();
	});
}

// ── Tray ──────────────────────────────────────────────────────────────────────
function createTray() {
	const trayPath = path.join(__dirname, "../../dist/tray.png");
	const icon = fs.existsSync(trayPath)
		? nativeImage.createFromPath(trayPath)
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
function ensureOutputFolder(): string {
	let f = store.get("outputFolder");
	if (!f) {
		f = path.join(app.getPath("videos"), "Clipsta");
		store.set("outputFolder", f);
	}
	if (!fs.existsSync(f)) fs.mkdirSync(f, { recursive: true });
	return f;
}

// ── Desktop device ID ─────────────────────────────────────────────────────────
function ensureDesktopDeviceId() {
	let id = store.get("desktopDeviceId");
	if (!id) {
		id = `desktop_${crypto.randomUUID().replace(/-/g, "")}`;
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
		if (powerBlockId !== null) powerSaveBlocker.stop(powerBlockId);
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
	const filePath = path.join(ensureOutputFolder(), fileName);
	await fs.promises.writeFile(filePath, Buffer.from(arrayBuffer));
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

		const enc = getEncoderArgs(opts.encoder, _encoderProbe ?? undefined);
		args.push("-c:v", enc.codec, ...enc.extra);
		if (opts.fps) args.push("-r", String(opts.fps));
		args.push("-c:a", "aac", "-b:a", "192k");
		args.push("-movflags", "+faststart", "-y", outputPath);

		const ffmpeg = getFfmpegPath();
		return new Promise((resolve, reject) => {
			execFile(ffmpeg, args, { timeout: 300000, maxBuffer: 100 * 1024 * 1024 }, (err2, _stdout, stderr) => {
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

ipcMain.handle("dialog:importFolder", async () => {
	const r = await dialog.showOpenDialog(mainWindow!, {
		properties: ["openDirectory"],
		title: "Select a folder of video clips to import",
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

// ── IPC: list audio devices via Rust WASAPI enumeration ───────────────────────
ipcMain.handle("audio:listDevices", async () => {
	const bin = getCaptureBinPath();
	return new Promise<{ id: string; kind: string }[]>((resolve) => {
		const proc = spawn(bin, ["list-audio-devices"], { stdio: ["ignore", "pipe", "pipe"] });
		let out = "";
		proc.stdout?.on("data", (d: Buffer) => { out += d.toString(); });
		proc.on("close", (code) => {
			if (code === 0 && out) {
				try { resolve(JSON.parse(out)); } catch { resolve([]); }
			} else { resolve([]); }
		});
		proc.on("error", () => resolve([]));
		setTimeout(() => { try { proc.kill(); } catch {} resolve([]); }, 5000);
	});
});

// ── IPC: open paths ───────────────────────────────────────────────────────────
ipcMain.handle("shell:openFolder", (_e, p: string) => shell.openPath(p));
ipcMain.handle("shell:openFile", (_e, p: string) => shell.openPath(p));
ipcMain.handle("shell:showItem", (_e, p: string) => shell.showItemInFolder(p));

// ── IPC: clip library ─────────────────────────────────────────────────────────
ipcMain.handle("clips:list", async () => {
	const folder = store.get("outputFolder");
	if (!folder || !fs.existsSync(folder)) return [];
	const entries = await fs.promises.readdir(folder, { withFileTypes: true });
	const results: { name: string; path: string; size: number; createdAt: string }[] = [];
	for (const entry of entries) {
		if (!/\.(webm|mp4|mkv|mov)$/i.test(entry.name)) continue;
		const full = path.join(folder, entry.name);
		try {
			const stat = await fs.promises.stat(full);
			results.push({ name: entry.name, path: full, size: stat.size, createdAt: stat.birthtime.toISOString() });
		} catch { /* file may have been deleted between readdir and stat */ }
	}
	return results.sort((a, b) => +new Date(b.createdAt) - +new Date(a.createdAt));
});

ipcMain.handle("clips:delete", async (_e, p: string) => {
	if (fs.existsSync(p)) await fs.promises.unlink(p);
	return true;
});

ipcMain.handle("clips:rename", async (_e, oldPath: string, newName: string) => {
	const dir = path.dirname(oldPath);
	const newPath = path.join(dir, newName);
	await fs.promises.rename(oldPath, newPath);
	return newPath;
});

ipcMain.handle("clips:import", async (_e, sourcePath: string) => {
	const folder = ensureOutputFolder();
	const name = path.basename(sourcePath);
	const dest = path.join(folder, name);
	if (fs.existsSync(dest)) {
		const ext = path.extname(name);
		const base = path.basename(name, ext);
		let i = 1;
		while (fs.existsSync(path.join(folder, `${base} (${i})${ext}`))) i++;
		const finalDest = path.join(folder, `${base} (${i})${ext}`);
		await fs.promises.copyFile(sourcePath, finalDest);
		return finalDest;
	}
	await fs.promises.copyFile(sourcePath, dest);
	return dest;
});

ipcMain.handle("clips:importFolder", async (_e, sourceFolder: string) => {
	const folder = ensureOutputFolder();
	const imported: string[] = [];
	const entries = await fs.promises.readdir(sourceFolder);
	const files = entries.filter((f) => /\.(webm|mp4|mkv|mov)$/i.test(f));
	for (const f of files) {
		const src = path.join(sourceFolder, f);
		const dest = path.join(folder, f);
		const ext = path.extname(f);
		const base = path.basename(f, ext);
		if (fs.existsSync(dest)) {
			let i = 1;
			while (fs.existsSync(path.join(folder, `${base} (${i})${ext}`))) i++;
			const destPath = path.join(folder, `${base} (${i})${ext}`);
			await fs.promises.copyFile(src, destPath);
			imported.push(destPath);
		} else {
			await fs.promises.copyFile(src, dest);
			imported.push(dest);
		}
	}
	return imported;
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
ipcMain.handle("file:read", async (_e, filePath: string) => {
	const buf = await fs.promises.readFile(filePath);
	return buf.buffer;
});

ipcMain.handle("file:stat", async (_e, filePath: string) => {
	const s = await fs.promises.stat(filePath);
	return { size: s.size, modifiedAt: s.mtime.toISOString() };
});

ipcMain.handle("file:ensureDir", async (_e, dirPath: string) => {
	await fs.promises.mkdir(dirPath, { recursive: true });
	return true;
});

ipcMain.handle("file:copyToDownloads", async (_e, filePath: string) => {
	const downloads = app.getPath("downloads");
	const name = path.basename(filePath);
	const ext = path.extname(name);
	const base = path.basename(name, ext);
	let dest = path.join(downloads, name);
	for (let i = 1; fs.existsSync(dest); i++) {
		dest = path.join(downloads, `${base} (${i})${ext}`);
	}
	await fs.promises.copyFile(filePath, dest);
	return dest;
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
	encoder?: string;
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
			const enc = getEncoderArgs(opts.encoder, _encoderProbe ?? undefined);
			exportArgs.push("-c:v", enc.codec, ...enc.extra);
			exportArgs.push("-c:a", "aac", "-b:a", "192k");
			exportArgs.push("-movflags", "+faststart", "-y", cleanupPath);

			await new Promise<void>((resolve, reject) => {
				const ffmpeg = getFfmpegPath();
				execFile(ffmpeg, exportArgs, { timeout: 300000, maxBuffer: 100 * 1024 * 1024 }, (err2, _stdout, stderr) => {
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
		const fileBuf = await fs.promises.readFile(uploadPath);
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

// ── WGC capture integration ───────────────────────────────────────────────────

interface WgcSourceInfo {
	id: string;
	name: string;
	source_type: string;
	width: number;
	height: number;
}

// ── WGC session state ─────────────────────────────────────────────────────────
let wgcProcess: ChildProcess | null = null;
let wgcOutputPath: string | null = null;
let wgcAudioPcmPath: string | null = null;
let wgcIsRecording = false;

let wgcSaving = false;
let wgcAudioSampleRate = 48000;
let wgcAudioChannels = 2;
let wgcCaptureWidth = 1920;
let wgcCaptureHeight = 1080;
let wgcCaptureFps = 60;
let wgcHasAudio = false;
let wgcRecordingStartTime = 0;
let wgcRecordingFrameCount = 0;

// ── WGC logging helpers (main + forward to renderer DevTools via IPC) ────
function wgcFwdSend(level: string, line: string) {
	try { mainWindow?.webContents?.send("wgc:log", { level, line }); } catch {}
}
function wgcLog(...args: any[]) {
	const ts = new Date().toISOString().slice(11, 19);
	const line = `[wgc ${ts}] ${args.map(a => a instanceof Error ? `${a.name}: ${a.message}` : typeof a === 'object' ? JSON.stringify(a) : String(a)).join(' ')}`;
	console.log(line);
	wgcFwdSend("log", line);
}
function wgcErr(...args: any[]) {
	const ts = new Date().toISOString().slice(11, 19);
	const line = `[wgc ${ts}] ${args.map(a => a instanceof Error ? `${a.name}: ${a.message}` : typeof a === 'object' ? JSON.stringify(a) : String(a)).join(' ')}`;
	console.error(line);
	wgcFwdSend("error", line);
}

/** Resolve the path to clipsta-capture.exe */
function getCaptureBinPath(): string {
	const inResources = path.join(process.resourcesPath, "clipsta-capture.exe");
	if (fs.existsSync(inResources)) return inResources;
	const inDev = path.join(__dirname, "../../bin/clipsta-capture.exe");
	if (fs.existsSync(inDev)) return inDev;
	return "clipsta-capture";
}

/** Convert resolution setting string to pixel dimensions */
function resolveTargetRes(setting: string): { w: number; h: number } | null {
	switch (setting) {
		case "480p":  return { w: 854,  h: 480 };
		case "720p":  return { w: 1280, h: 720 };
		case "1080p": return { w: 1920, h: 1080 };
		case "1440p": return { w: 2560, h: 1440 };
		case "4k":    return { w: 3840, h: 2160 };
		default:      return null;
	}
}

/**
 * Extract a segment from a complete muxed MP4 via stream copy (no re-encode).
 */
async function extractClipFromSyncedMp4(
	sourcePath: string,
	outputPath: string,
	seekFrom: number,
	duration: number,
): Promise<void> {
	const ffmpeg = getFfmpegPath();
	return new Promise((resolve, reject) => {
		const args: string[] = [
			"-ss", String(seekFrom),
			"-t", String(duration),
			"-i", sourcePath,
			"-c", "copy",
			"-movflags", "+faststart",
			"-y", outputPath,
		];
		const proc = spawn(ffmpeg, args, { stdio: ["ignore", "pipe", "pipe"] });
		let stderr = "";
		proc.stderr?.on("data", (d: Buffer) => { stderr += d.toString(); });
		proc.on("close", (code) => {
			if (code === 0) resolve();
			else reject(new Error(`extractClip failed (code ${code}): ${stderr.slice(-200)}`));
		});
		proc.on("error", reject);
	});
}

/**
 * Wait for a file to be fully written and unlocked.
 * On Windows, opening for read doesn't conflict with a writer, so we need to
 * try opening with write access (which WILL conflict) to detect if another
 * process still has the file open for writing.
 * Returns true if the file is ready, false if timeout.
 */
async function waitForFileReady(filePath: string, timeoutMs: number): Promise<boolean> {
	const startTime = Date.now();
	const pollInterval = 500;
	while (Date.now() - startTime < timeoutMs) {
		try {
			// Try to open with r+ (read-write) — this will fail with EBUSY if
			// another process (FFmpeg) still has the file open for writing
			const fd = fs.openSync(filePath, "r+");
			const stat = fs.fstatSync(fd);
			fs.closeSync(fd);
			// File must exist and have meaningful content
			if (stat.size > 1024) {
				wgcLog("waitForFileReady: file ready", { size: stat.size, waitMs: Date.now() - startTime });
				return true;
			}
		} catch (e: any) {
			// EBUSY, EPERM, EACCES means file is still locked — keep waiting
			if (e.code === "ENOENT") {
				wgcErr("waitForFileReady: file does not exist");
				return false;
			}
			// Any lock-related error: keep polling
		}
		await new Promise((r) => setTimeout(r, pollInterval));
	}
	wgcErr("waitForFileReady: timeout waiting for file to be ready");
	return false;
}

// ── IPC: list WGC sources ─────────────────────────────────────────────────────
ipcMain.handle("wgc:sources", async (): Promise<WgcSourceInfo[]> => {
	const captureBin = getCaptureBinPath();
	return new Promise((resolve) => {
		let output = "";
		try {
			const proc = spawn(captureBin, ["list-sources"], { stdio: ["ignore", "pipe", "ignore"] });
			proc.stdout?.on("data", (d: Buffer) => { output += d.toString(); });
			proc.on("close", () => {
				try { resolve(JSON.parse(output)); } catch { resolve([]); }
			});
			proc.on("error", () => resolve([]));
			setTimeout(() => { try { proc.kill(); } catch { /* */ } resolve([]); }, 5000);
		} catch { resolve([]); }
	});
});

// ── IPC: start WGC recording ──────────────────────────────────────────────────
ipcMain.handle("wgc:startRecording", async (_e, opts: {
	sourceId: string | null;
	fps: number;
	noAudio: boolean;
	micDevice?: string;
	loopbackDevice?: string;
}): Promise<{ width: number; height: number; fps: number } | null> => {
	if (wgcIsRecording) {
		wgcLog("startRecording blocked — already recording");
		return null;
	}
	if (wgcSaving) {
		wgcLog("startRecording blocked — saveClip in progress");
		return null;
	}

	if (!_encoderProbe) {
		wgcLog("startRecording: waiting for encoder probe…");
		await probeEncoders();
	}

	const captureBin = getCaptureBinPath();
	const ffmpegPath = getFfmpegPath();
	const outputFolder = ensureOutputFolder();

	const now = new Date();
	const pad2 = (n: number) => String(n).padStart(2, "0");
	const stamp = `${now.getFullYear()}${pad2(now.getMonth()+1)}${pad2(now.getDate())}_${pad2(now.getHours())}${pad2(now.getMinutes())}${pad2(now.getSeconds())}`;

	const tempDir = app.getPath("temp");
	wgcOutputPath    = path.join(tempDir, `clipsta_rec_${stamp}.mp4`);
	wgcHasAudio      = !opts.noAudio;

	wgcLog("starting recording", { sourceId: opts.sourceId, fps: opts.fps, hasAudio: wgcHasAudio, mic: opts.micDevice, bin: captureBin, out: wgcOutputPath });

	const bitrateKbps = store.get("bitrate") ?? 25000;
	const resSetting: string | undefined = store.get("resolution");
	let targetW = 0, targetH = 0;
	if (resSetting) {
		const t = resolveTargetRes(resSetting);
		if (t) { targetW = t.w; targetH = t.h; }
	}

	const captureArgs = ["capture"];
	if (opts.sourceId) captureArgs.push("--source", opts.sourceId);
	captureArgs.push("--fps", String(opts.fps ?? 60));
	if (opts.noAudio) captureArgs.push("--no-audio");
	if (opts.micDevice) captureArgs.push("--mic-device", opts.micDevice);
	if (opts.loopbackDevice) captureArgs.push("--loopback-device", opts.loopbackDevice);
	captureArgs.push("--bitrate", String(bitrateKbps));
	captureArgs.push("--output", wgcOutputPath);
	if (targetW > 0) { captureArgs.push("--width", String(targetW)); captureArgs.push("--height", String(targetH)); }

	wgcLog("capture args:", captureArgs.join(" "));

	return new Promise<{ width: number; height: number; fps: number } | null>((resolve) => {
		let readyTimer: ReturnType<typeof setTimeout> | null = null;
		try {
			wgcProcess = spawn(captureBin, captureArgs, {
				stdio: ["pipe", "pipe", "pipe"],
			});
			wgcLog("spawned pid", wgcProcess.pid);

			if (!wgcProcess || !wgcProcess.stdout || !wgcProcess.stderr) {
				wgcErr("spawn returned null or missing stdio");
				resolve(null);
				return;
			}

			let buf = "";
			let readyReceived = false;
			readyTimer = setTimeout(() => {
				if (!readyReceived) {
					wgcErr("ready timeout — Rust did not send ready within 10s");
					try { wgcProcess?.kill(); } catch {}
					resolve(null);
				}
			}, 10_000);

			// stdout: one JSON line per message
			wgcProcess.stdout.on("data", (chunk: Buffer) => {
				buf += chunk.toString("utf8");
				const lines = buf.split("\n");
				buf = lines.pop() ?? "";
				for (const line of lines) {
					const trimmed = line.trim();
					if (!trimmed) continue;
					try {
						const msg = JSON.parse(trimmed);
						if (msg.status === "ready" && !readyReceived) {
							readyReceived = true;
							if (readyTimer) { clearTimeout(readyTimer); readyTimer = null; }
							wgcCaptureWidth    = msg.width;
							wgcCaptureHeight   = msg.height;
							wgcCaptureFps      = msg.fps;
							wgcRecordingStartTime = Date.now();
							wgcLog("ready", `${msg.width}x${msg.height} @ ${msg.fps}fps`);
							resolve({ width: msg.width, height: msg.height, fps: msg.fps });
						} else if (msg.status === "done") {
							wgcIsRecording = false;
							if (msg.frames) wgcRecordingFrameCount = msg.frames;
							wgcLog("done received", { frames: msg.frames, output: msg.output });
						} else if (msg.status === "fatal") {
							wgcErr("Rust fatal:", msg.error);
						}
					} catch {}
				}
			});

			wgcProcess.stdout.on("error", (err) => wgcErr("stdout stream error:", err.message));
			wgcProcess.stderr.on("error", (err) => wgcErr("stderr stream error:", err.message));

			// stderr: ffmpeg + Rust diagnostics
			wgcProcess.stderr.on("data", (chunk: Buffer) => {
				const text = chunk.toString("utf8").trim();
				if (text) wgcErr("Rust:", text);
			});

			// Process exit: output file is ready
			wgcProcess.on("close", (code, signal) => {
				wgcIsRecording = false;
				wgcRecordingStartTime = 0;
				wgcLog("process closed", { code, signal });
				if (code !== 0 && code !== null) {
					wgcErr("process exited with non-zero code", code, signal);
				}
				// Clean up temp recording file if it exists (unless saveClip already handled it)
				if (wgcOutputPath) {
					const tempFile = wgcOutputPath;
					setTimeout(() => {
						try { if (fs.existsSync(tempFile)) fs.unlinkSync(tempFile); } catch {}
					}, 2000);
				}
				if (wgcAudioPcmPath) {
					try { if (fs.existsSync(wgcAudioPcmPath)) fs.unlinkSync(wgcAudioPcmPath); } catch {}
				}
				wgcOutputPath = null;
				wgcAudioPcmPath = null;
			});

			wgcProcess.on("error", (err) => {
				wgcErr("spawn error:", err.message);
				wgcIsRecording = false;
				if (!readyReceived) { if (readyTimer) { clearTimeout(readyTimer); readyTimer = null; } resolve(null); }
			});

			wgcIsRecording = true;
			isRecording = true;
			refreshTrayMenu();
			if (powerBlockId !== null) powerSaveBlocker.stop(powerBlockId);
			powerBlockId = powerSaveBlocker.start("prevent-display-sleep");
		} catch (err) {
			wgcErr("startRecording error:", err);
			if (readyTimer) { clearTimeout(readyTimer); readyTimer = null; }
			resolve(null);
		}
	});
});

// ── IPC: stop WGC recording ───────────────────────────────────────────────────
ipcMain.handle("wgc:stopRecording", async () => {
	if (!wgcProcess) {
		wgcLog("stopRecording called but no process to stop");
		return;
	}

	wgcLog("sending stop command");
	wgcIsRecording = false;
	isRecording = false;
	refreshTrayMenu();
	if (powerBlockId !== null) {
		powerSaveBlocker.stop(powerBlockId);
		powerBlockId = null;
	}

	try { wgcProcess.stdin?.write(JSON.stringify({ cmd: "stop" }) + "\n"); } catch (e) { wgcErr("stdin write error:", e); }

	// Force kill after 5 seconds if it hasn't exited — but only if wgcProcess hasn't changed
	const procToKill = wgcProcess;
	const procPid = procToKill?.pid;
	setTimeout(() => {
		// Only kill/null if this is still the same process (not a newly spawned one)
		if (procToKill && !procToKill.killed && wgcProcess === procToKill) {
			wgcLog("force kill after timeout");
			try { procToKill.kill(); } catch { }
			wgcProcess = null;
			wgcIsRecording = false;
		}
	}, 5000);
});

// ── IPC: save full recording — stop WGC and finalize the whole file ────────
ipcMain.handle("wgc:saveFullRecording", async (): Promise<string | null> => {
	if (!wgcProcess || !wgcOutputPath) {
		wgcLog("saveFullRecording: nothing to save");
		return null;
	}
	const tempOutput = wgcOutputPath;
	wgcOutputPath = null; // Prevent on-close handler from deleting the file we need

	wgcLog("saveFullRecording: stopping Rust (internal encoder already muxed the output)");
	wgcIsRecording = false;
	isRecording = false;
	refreshTrayMenu();
	if (powerBlockId !== null) {
		powerSaveBlocker.stop(powerBlockId);
		powerBlockId = null;
	}

	try { wgcProcess.stdin?.write(JSON.stringify({ cmd: "stop" }) + "\n"); } catch (e) {
		wgcErr("saveFullRecording: stdin write error:", e);
	}

	// Wait for process exit (Rust's internal FFmpeg has already written the muxed file)
	await new Promise<void>((resolve) => {
		if (!wgcProcess || wgcProcess.killed) return resolve();
		const onClose = () => { resolve(); };
		wgcProcess.once("close", onClose);
		setTimeout(() => {
			wgcProcess?.removeListener("close", onClose);
			resolve();
		}, 30000);
	});

	// Wait for file to be fully written and unlocked
	await waitForFileReady(tempOutput, 10000);

	if (fs.existsSync(tempOutput) && fs.statSync(tempOutput).size > 1024) {
		// Media Foundation already outputs a valid MP4 — just move it to the clips folder
		const now = new Date();
		const pad2 = (n: number) => String(n).padStart(2, "0");
		const stamp = `${now.getFullYear()}${pad2(now.getMonth()+1)}${pad2(now.getDate())}_${pad2(now.getHours())}${pad2(now.getMinutes())}${pad2(now.getSeconds())}`;
		const finalPath = path.join(ensureOutputFolder(), `Clipsta_full_${stamp}.mp4`);
		try {
			fs.renameSync(tempOutput, finalPath);
		} catch {
			fs.copyFileSync(tempOutput, finalPath);
			try { fs.unlinkSync(tempOutput); } catch {}
		}
		wgcLog("saveFullRecording: done:", finalPath);
		if (mainWindow && !mainWindow.isDestroyed()) {
			mainWindow.webContents.send("wgc:clipSaved", finalPath);
		}
		return finalPath;
	}
	wgcErr("saveFullRecording: output file missing or too small");
	return null;
});

// ── IPC: save clip — self-contained capture OR extract from ongoing recording ─
ipcMain.handle("wgc:saveClip", async (_e, opts: {
	seconds: number;
	fileName: string;
	sourceId: string | null;
	fps?: number;
	noAudio?: boolean;
	micDevice?: string;
	loopbackDevice?: string;
}): Promise<string | null> => {
	const { seconds, fileName, sourceId, fps = 60, noAudio = false, micDevice, loopbackDevice } = opts;
	const outputPath = path.join(ensureOutputFolder(), fileName);

	if (wgcSaving) {
		wgcErr("saveClip blocked — another save is already in progress");
		return null;
	}
	wgcSaving = true;

	// ── Recording is active: stop → extract → restart ──
	// Media Foundation's MP4 files can't be read while being written (moov atom at EOF).
	// So we stop the recording, extract the clip, then restart immediately.
	if (wgcIsRecording && wgcOutputPath && fs.existsSync(wgcOutputPath)) {
		const recordingDuration = (Date.now() - wgcRecordingStartTime) / 1000;
		const clipDuration = Math.min(seconds, Math.max(0, recordingDuration - 1.0));
		const seekFrom = Math.max(0, recordingDuration - 1.0 - clipDuration);
		wgcLog("saveClip: stop → extract → restart", { seekFrom, clipDuration, recordingDuration });

		if (clipDuration < 1) {
			wgcErr("saveClip: not enough recording time yet");
			wgcSaving = false;
			return null;
		}

		// Save refs before stopping
		const sourcePath = wgcOutputPath;
		const captureOpts = {
			sourceId: opts.sourceId,
			fps: opts.fps,
			noAudio: opts.noAudio,
			micDevice: opts.micDevice,
			loopbackDevice: opts.loopbackDevice,
		};
		wgcOutputPath = null; // Prevent close handler from deleting

		// Stop the recording process
		wgcIsRecording = false;
		isRecording = false;
		if (powerBlockId !== null) { powerSaveBlocker.stop(powerBlockId); powerBlockId = null; }
		try { wgcProcess?.stdin?.write(JSON.stringify({ cmd: "stop" }) + "\n"); } catch {}

		// Wait for process to fully exit and file to be finalized
		await new Promise<void>((resolve) => {
			if (!wgcProcess || wgcProcess.killed) return resolve();
			wgcProcess.once("close", () => resolve());
			setTimeout(() => resolve(), 15000);
		});

		// Wait for file to be unlocked
		await waitForFileReady(sourcePath, 5000);

		// Extract the clip
		let extractSuccess = false;
		if (fs.existsSync(sourcePath) && fs.statSync(sourcePath).size > 1024) {
			try {
				const ffmpeg = getFfmpegPath();
				await new Promise<void>((resolve, reject) => {
					// Use -ss AFTER -i for frame-accurate seeking (avoids keyframe misalignment).
					// Video: copy (fast, keyframe-aligned)
					// Audio: re-encode AAC (ensures audio starts at the exact same point as video)
					// -avoid_negative_ts make_zero: prevents audio from starting before video
					const args = [
						"-i", sourcePath,
						"-ss", String(seekFrom),
						"-t", String(clipDuration),
						"-c:v", "copy",
						"-c:a", "aac", "-b:a", "192k",
						"-avoid_negative_ts", "make_zero",
						"-movflags", "+faststart",
						"-y", outputPath,
					];
					wgcLog("saveClip: ffmpeg extract args:", args.join(" "));
					const proc = spawn(ffmpeg, args, { stdio: ["ignore", "pipe", "pipe"] });
					let stderr = "";
					proc.stderr?.on("data", (d: Buffer) => { stderr += d.toString(); });
					proc.on("close", (code) => {
						if (code === 0) resolve();
						else reject(new Error(`extract failed (code ${code}): ${stderr.slice(-200)}`));
					});
					proc.on("error", reject);
				});
				extractSuccess = true;
				wgcLog("saveClip: clip saved:", outputPath);
				if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send("wgc:clipSaved", outputPath);
			} catch (err) {
				wgcErr("saveClip: extraction failed:", err);
			}
		}

		// Clean up the temp recording file
		try { if (fs.existsSync(sourcePath)) fs.unlinkSync(sourcePath); } catch {}

		// Restart recording immediately by signaling the renderer
		wgcSaving = false;
		refreshTrayMenu();
		if (mainWindow && !mainWindow.isDestroyed()) {
			// Small delay to let the process fully clean up
			setTimeout(() => {
				mainWindow?.webContents.send("hotkey:record");
			}, 500);
		}

		return extractSuccess ? outputPath : null;
	}

	// ── Self-contained capture (no recording active) ────────────────────
	wgcLog("saveClip: self-contained capture", { seconds, fileName, sourceId, noAudio, micDevice, loopbackDevice });

	const captureBin = getCaptureBinPath();
	const ffmpegPath = getFfmpegPath();
	const tempDir = app.getPath("temp");
	const stamp = Date.now();
	const enc = getEncoderArgs(store.get("encoder"), _encoderProbe ?? undefined);
	const encoderArgsJson = JSON.stringify(enc);
	const resSetting: string | undefined = store.get("resolution");
	let targetW = 0, targetH = 0;
	if (resSetting) {
		const t = resolveTargetRes(resSetting);
		if (t) { targetW = t.w; targetH = t.h; }
	}

	return await new Promise<string | null>((resolve) => {
		const args = ["capture"];
		if (sourceId) args.push("--source", sourceId);
		args.push("--fps", String(fps));
		if (noAudio) args.push("--no-audio");
		if (micDevice) args.push("--mic-device", micDevice);
		if (loopbackDevice) args.push("--loopback-device", loopbackDevice);
		args.push("--bitrate", String(store.get("bitrate") ?? 25000));
		args.push("--output", outputPath);
		if (targetW > 0) { args.push("--width", String(targetW)); args.push("--height", String(targetH)); }

		wgcLog("saveClip args:", args.join(" "));

		let killTimer: ReturnType<typeof setTimeout> | null = setTimeout(() => {
			try { if (proc && !proc.killed) proc.kill(); } catch {}
		}, (seconds + 10) * 1000);

		let proc: ChildProcess | null = null;
		try {
			proc = spawn(captureBin, args, { stdio: ["pipe", "pipe", "pipe"] });
			if (!proc || !proc.stdout) { wgcErr("saveClip: spawn failed"); wgcSaving = false; resolve(null); return; }

			proc.stdout.on("data", () => { }); // drain stdout

			proc.stderr?.on("data", (chunk: Buffer) => {
				const text = chunk.toString("utf8").trim();
				if (text) wgcFwdSend("error", `[saveClip Rust] ${text}`);
			});
			proc.stderr?.on("error", (err) => wgcErr("saveClip stderr error:", err.message));

			proc.on("error", (err) => {
				wgcErr("saveClip: spawn error:", err.message);
				wgcSaving = false; resolve(null);
			});

			proc.on("close", (code) => {
				if (killTimer) { clearTimeout(killTimer); killTimer = null; }
				wgcSaving = false;
				wgcLog("saveClip: process closed, code:", code);

				if (fs.existsSync(outputPath) && fs.statSync(outputPath).size > 1024) {
					wgcLog("saveClip: saved:", outputPath);
					if (mainWindow && !mainWindow.isDestroyed()) mainWindow.webContents.send("wgc:clipSaved", outputPath);
					resolve(outputPath);
				} else {
					wgcErr("saveClip: output file missing or too small");
					resolve(null);
				}
			});

			setTimeout(() => {
				if (!proc?.killed) {
					try { proc?.stdin?.write(JSON.stringify({ cmd: "stop" }) + "\n"); } catch {}
				}
			}, seconds * 1000);

		} catch (err) {
			wgcErr("saveClip: setup error:", err);
			wgcSaving = false; resolve(null);
		}
	});
});







interface ExportOpts {
	format: string;
	aspectRatio: string;
	resolution: string;
	encoder?: string;
	fps?: number;
	trimStart?: number;
	trimEnd?: number;
	cuts?: { start: number; end: number }[];
	timeline?: { path: string; trimIn: number; trimOut: number }[];
}
