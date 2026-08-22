import { useEffect, useRef, useState } from "react";
import React from "react";
import { Film, Scissors, Settings, Zap, Upload } from "lucide-react";
import type { Page } from "./types";
import { useSettings } from "./hooks/useSettings";
import { useRecorder } from "./hooks/useRecorder";
import { useCloudUpload } from "./hooks/useCloudUpload";
import TitleBar from "./components/TitleBar";
import LibraryPage from "./components/pages/LibraryPage";
const EditorPage = React.lazy(() => import("./components/pages/EditorPage"));
import SettingsPage from "./components/pages/SettingsPage";
import StatusBar from "./components/StatusBar";
import SaveNotification from "./components/SaveNotification";
import ExportToast from "./components/ExportToast";
import CaptureRecoveryToast from "./components/CaptureRecoveryToast";
import bridge from "./tauri-bridge";

export default function App() {
	const [page, setPage] = useState<Page>("capture");
	const [editorFile, setEditorFile] = useState<string | null>(null);
	const [exportedFile, setExportedFile] = useState<string | null>(null);
	const { settings, loaded, updateSetting, saveAll } = useSettings();
	const recorder = useRecorder(loaded ? settings : null);
	const cloud = useCloudUpload(loaded ? settings : null);

	const openInEditor = (filePath: string) => {
		// Force a reset by clearing first, then setting — ensures useEffect fires even for same file
		setEditorFile(null);
		setTimeout(() => {
			setEditorFile(filePath);
			setPage("editor");
		}, 0);
	};

	const uploadCount = cloud.queue.filter((j) => j.status === "queued" || j.status === "uploading").length;

	// Auto-upload on clip save
	const prevSavedPathRef = useRef<string | null>(null);
	const addToQueueRef = useRef(cloud.addToQueue);
	addToQueueRef.current = cloud.addToQueue;
	const settingsRef = useRef(settings);
	settingsRef.current = settings;
	const cloudPairedRef = useRef(cloud.paired);
	cloudPairedRef.current = cloud.paired;

	useEffect(() => {
		const path = recorder.state.savedPath;
		if (!path || path === prevSavedPathRef.current) return;
		prevSavedPathRef.current = path;

		// Auto-upload: wait for file to be fully written, then queue
		const tryUpload = async () => {
			const s = settingsRef.current;
			if (!s?.autoUpload || !s?.cloudEnabled) return;

			const name = path.split("\\").pop() ?? path.split("/").pop() ?? path;
			// Retry getting file stats up to 3 times with increasing delays
			for (let attempt = 0; attempt < 3; attempt++) {
				await new Promise((r) => setTimeout(r, 1000 + attempt * 1000));
				try {
					const stat = await bridge.getFileStats(path);
					if (stat.size > 0) {
						addToQueueRef.current(path, name, stat.size);
						return;
					}
				} catch { /* file not ready yet */ }
			}
		};
		tryUpload();
	}, [recorder.state.savedPath]);

	// Forward Tauri backend logs to DevTools
	useEffect(() => {
		const unlistenPromise = bridge.onWgcLog((level, line) => {
			if (level === "error") console.error(line);
			else console.log(line);
		});
		return () => {
			if (unlistenPromise && typeof unlistenPromise === "object" && "then" in unlistenPromise) {
				(unlistenPromise as Promise<() => void>).then((u) => u()).catch(() => {});
			}
		};
	}, []);

	return (
		<div className="flex flex-col h-screen bg-bg overflow-hidden" data-theme={settings.theme ?? "dark"}>
			<TitleBar />

			<div className="flex flex-1 overflow-hidden">
				{/* Sidebar */}
				<aside className="w-[200px] flex-shrink-0 bg-[#0d0d0d] border-r border-border flex flex-col py-4">
					<div className="px-4 mb-6">
						<div className="flex items-center gap-2">
							<Zap size={18} className="text-y fill-y" />
							<span className="text-white font-black text-lg tracking-tight">CLIPSTA</span>
						</div>
						<p className="text-text-dim text-[10px] mt-0.5">Always recording. Always ready.</p>
					</div>

					<nav className="flex-1 px-2 space-y-1">
						<NavItem icon={<Zap size={16} />} label="Status" active={page === "capture"} onClick={() => setPage("capture")} />
						<NavItem icon={<Film size={16} />} label="Library" active={page === "library"} onClick={() => setPage("library")} />
						<NavItem icon={<Scissors size={16} />} label="Editor" active={page === "editor"} onClick={() => setPage("editor")} />
						<NavItem icon={<Settings size={16} />} label="Settings" active={page === "settings"} onClick={() => setPage("settings")} />
					</nav>

					{cloud.paired && uploadCount > 0 && (
						<div className="px-3 mb-2">
							<button onClick={() => setPage("library")} className="w-full rounded-lg px-3 py-2 border border-y/30 bg-[#1c1c00] flex items-center gap-2">
								<Upload size={12} className="text-y" />
								<span className="text-y text-[10px] font-bold">{uploadCount} upload{uploadCount !== 1 ? "s" : ""} pending</span>
							</button>
						</div>
					)}

					<div className="px-3 mt-4 space-y-2">
						<RecordingIndicator status={recorder.state.status} duration={recorder.state.duration} />
					</div>
				</aside>

				{/* Content */}
				<main className="flex-1 overflow-hidden flex flex-col">
					<div className="flex-1 overflow-hidden slide-in">
						{page === "capture" && <StatusPage recorder={recorder} settings={settings} />}
						{page === "library" && <LibraryPage onOpenEditor={openInEditor} cloud={cloud} />}
						<div style={{ display: page === "editor" ? "flex" : "none", height: "100%", overflow: "hidden" }}>
							<React.Suspense fallback={<div className="flex-1 flex items-center justify-center text-text-dim">Loading editor...</div>}>
								<EditorPage initialFile={editorFile} settings={settings} cloud={cloud} onExportDone={setExportedFile} />
							</React.Suspense>
						</div>
						{page === "settings" && <SettingsPage settings={settings} updateSetting={updateSetting} saveAll={saveAll} cloud={cloud} />}
					</div>
					<StatusBar recorder={recorder} settings={settings} />
				</main>
			</div>

			<SaveNotification path={recorder.state.savedPath} />
			<ExportToast path={exportedFile} />
			<CaptureRecoveryToast />
		</div>
	);
}

// ── Status Page ─────────────────────────────────────────────────────────────
function StatusPage({ recorder, settings }: { recorder: any; settings: any }) {
	const { status, duration, error, captureWidth, captureHeight, captureFps } = recorder.state;
	const isActive = status === "recording";
	const bufferDuration = settings?.bufferDuration ?? 60;
	const bufferFill = isActive ? Math.min(duration / bufferDuration, 1) : 0;
	const bufferReady = duration >= 5;

	return (
		<div className="h-full flex flex-col items-center justify-center p-8 space-y-6">
			<div className="text-center space-y-3">
				<div className={`w-20 h-20 rounded-full mx-auto flex items-center justify-center ${isActive ? "bg-green-900/50 border-2 border-green-500" : "bg-yellow-900/30 border-2 border-yellow-500/50"}`}>
					<div className={`w-8 h-8 rounded-full ${isActive ? "bg-green-500 rec-pulse" : "bg-yellow-500/50 rec-pulse"}`} />
				</div>
				<h2 className={`text-2xl font-black ${isActive ? "text-green-400" : error ? "text-red-400" : "text-yellow-400"}`}>
					{isActive ? "RECORDING" : error ? "CAPTURE FAILED" : "STARTING..."}
				</h2>
				{isActive && !bufferReady && <p className="text-text-dim text-sm">Clip available in {5 - duration}s</p>}
				{isActive && bufferReady && <p className="text-text-dim text-sm">Buffer: <span className="text-white font-mono">{formatDur(duration)}</span> / {Math.floor(bufferDuration / 60)} min</p>}
				{error && !error.includes("available") && <p className="text-red-400 text-xs mt-2 animate-pulse">{error}</p>}
				{error && error.includes("available") && <p className="text-yellow-400 text-xs mt-2">{error}</p>}
			</div>

			{/* Buffer Fill Indicator */}
			{isActive && (
				<div className="w-full max-w-md space-y-1">
					<div className="flex items-center justify-between text-[10px] text-text-dim">
						<span>{bufferFill >= 1 ? "✓ Ready to clip" : "Filling buffer..."}</span>
						<span>{bufferFill >= 1 ? `${Math.floor(bufferDuration / 60)} min available` : `${Math.round(bufferFill * 100)}%`}</span>
					</div>
					<div className="h-1.5 bg-muted rounded-full overflow-hidden">
						<div
							className="h-full rounded-full transition-all duration-1000"
							style={{
								width: `${bufferFill * 100}%`,
								backgroundColor: bufferFill >= 1 ? "#22c55e" : "#D4F000",
							}}
						/>
					</div>
				</div>
			)}

			<div className="grid grid-cols-3 gap-3 w-full max-w-lg">
				<ClipButton label="Last 30s" hotkey={settings?.hotkeyClip30Sec || "Ctrl+Shift+G"} onClick={() => recorder.saveClip(30)} disabled={!isActive} />
				<ClipButton label="Last 1 Min" hotkey={settings?.hotkeyClip1Min || "Alt+F9"} onClick={() => recorder.saveClip(60)} disabled={!isActive} />
				<ClipButton label="Last 5 Min" hotkey={settings?.hotkeyClip5Min || "Alt+F10"} onClick={() => recorder.saveClip(300)} disabled={!isActive || bufferDuration < 300} />
			</div>

			{/* Capture Stats */}
			{isActive && captureWidth > 0 && (
				<div className="flex items-center gap-4 text-[11px] text-text-dim">
					<span className="flex items-center gap-1">
						<span className="w-1.5 h-1.5 rounded-full bg-green-500" />
						{captureWidth}×{captureHeight}
					</span>
					<span>{captureFps}fps</span>
					<span>H.264 HW</span>
					<span>{settings?.quality === "ultra" ? "Ultra" : settings?.quality === "high" ? "High" : "Standard"}</span>
				</div>
			)}

			<div className="text-center text-text-dim text-xs max-w-sm space-y-1">
				<p>Clipsta records continuously in the background.</p>
				<p>Press a hotkey or click a button to save your clip.</p>
				<p className="text-text-dim/60">Minimize to tray — recording continues.</p>
			</div>
		</div>
	);
}

function ClipButton({ label, hotkey, onClick, disabled }: { label: string; hotkey: string; onClick: () => void; disabled: boolean }) {
	return (
		<button onClick={onClick} disabled={disabled}
			className="flex flex-col items-center gap-1 p-4 rounded-xl border border-border bg-card hover:border-y hover:bg-y/5 transition-all disabled:opacity-30 disabled:cursor-not-allowed">
			<Zap size={20} className="text-y" />
			<span className="text-white text-sm font-bold">{label}</span>
			<span className="text-text-dim text-[10px]">{hotkey}</span>
		</button>
	);
}

function NavItem({ icon, label, active, onClick }: { icon: React.ReactNode; label: string; active: boolean; onClick: () => void }) {
	return (
		<button onClick={onClick} className={`nav-btn ${active ? "active" : ""}`}>
			{icon}<span>{label}</span>
			{active && <div className="ml-auto w-1 h-4 bg-y rounded-full" />}
		</button>
	);
}

function RecordingIndicator({ status, duration }: { status: string; duration: number }) {
	const isRec = status === "recording";
	return (
		<div className={`rounded-lg px-3 py-2 border ${isRec ? "border-green-800 bg-[#0a1a0a]" : "border-border bg-card"}`}>
			<div className="flex items-center gap-2">
				<div className={`w-2 h-2 rounded-full ${isRec ? "bg-green-500 rec-pulse" : "bg-text-dim"}`} />
				<span className={`text-xs font-bold ${isRec ? "text-green-400" : "text-text-dim"}`}>{isRec ? "BUFFER ACTIVE" : "STARTING..."}</span>
				{isRec && <span className="text-xs text-green-600 ml-auto font-mono">{formatDur(duration)}</span>}
			</div>
		</div>
	);
}

function formatDur(s: number) {
	const m = Math.floor(s / 60);
	const sec = s % 60;
	return `${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}`;
}
