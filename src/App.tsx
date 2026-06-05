import { useState } from "react";
import { Monitor, Film, Scissors, Settings, Zap, Cloud, Upload } from "lucide-react";
import type { Page } from "./types";
import { useSettings } from "./hooks/useSettings";
import { useRecorder } from "./hooks/useRecorder";
import { useCloudUpload } from "./hooks/useCloudUpload";
import TitleBar from "./components/TitleBar";
import CapturePage from "./components/pages/CapturePage";
import LibraryPage from "./components/pages/LibraryPage";
import EditorPage from "./components/pages/EditorPage";
import SettingsPage from "./components/pages/SettingsPage";
import StatusBar from "./components/StatusBar";
import SaveNotification from "./components/SaveNotification";

export default function App() {
	const [page, setPage] = useState<Page>("capture");
	const [editorFile, setEditorFile] = useState<string | null>(null);
	const { settings, loaded, updateSetting, saveAll } = useSettings();
	const recorder = useRecorder(loaded ? settings : null);
	const cloud = useCloudUpload(loaded ? settings : null);

	const openInEditor = (filePath: string) => {
		setEditorFile(filePath);
		setPage("editor");
	};

	const uploadCount = cloud.queue.filter((j) => j.status !== "done").length;

	return (
		<div className="flex flex-col h-screen bg-bg overflow-hidden">
			<TitleBar />

			<div className="flex flex-1 overflow-hidden">
				{/* ── Sidebar ── */}
				<aside className="w-[200px] flex-shrink-0 bg-[#0d0d0d] border-r border-border flex flex-col py-4">
					{/* Logo */}
					<div className="px-4 mb-6">
						<div className="flex items-center gap-2">
							<Zap size={18} className="text-y fill-y" />
							<span className="text-white font-black text-lg tracking-tight">CLIPSTA</span>
						</div>
						<p className="text-text-dim text-[10px] mt-0.5">Clip it before it's gone.</p>
					</div>

					{/* Nav */}
					<nav className="flex-1 px-2 space-y-1">
						<NavItem icon={<Monitor size={16} />} label="Capture" active={page === "capture"} onClick={() => setPage("capture")} />
						<NavItem icon={<Film size={16} />} label="Library" active={page === "library"} onClick={() => setPage("library")} />
						<NavItem icon={<Scissors size={16} />} label="Editor" active={page === "editor"} onClick={() => setPage("editor")} />
						<NavItem icon={<Settings size={16} />} label="Settings" active={page === "settings"} onClick={() => setPage("settings")} />
					</nav>

					{/* Upload queue indicator */}
					{cloud.paired && uploadCount > 0 && (
						<div className="px-3 mb-2">
							<button
								onClick={() => setPage("library")}
								className="w-full rounded-lg px-3 py-2 border border-y/30 bg-[#1c1c00] flex items-center gap-2"
							>
								<Upload size={12} className="text-y" />
								<span className="text-y text-[10px] font-bold">
									{uploadCount} upload{uploadCount !== 1 ? "s" : ""} pending
								</span>
							</button>
						</div>
					)}

					{/* Quick stats */}
					<div className="px-3 mt-4 space-y-2">
						<RecordingIndicator status={recorder.state.status} duration={recorder.state.duration} />
						<QuickClipButtons onSave1Min={() => recorder.saveClip(60)} onSave5Min={() => recorder.saveClip(300)} active={recorder.state.status === "recording"} />
					</div>
				</aside>

				{/* ── Content ── */}
				<main className="flex-1 overflow-hidden flex flex-col">
					<div className="flex-1 overflow-hidden slide-in">
						{page === "capture" && (
							<CapturePage recorder={recorder} settings={settings} />
						)}
						{page === "library" && (
							<LibraryPage onOpenEditor={openInEditor} cloud={cloud} />
						)}
						{page === "editor" && (
							<EditorPage initialFile={editorFile} settings={settings} />
						)}
						{page === "settings" && (
							<SettingsPage settings={settings} updateSetting={updateSetting} saveAll={saveAll} cloud={cloud} />
						)}
					</div>
					<StatusBar recorder={recorder} settings={settings} />
				</main>
			</div>

			{/* Global save notification */}
			<SaveNotification path={recorder.state.savedPath} />
		</div>
	);
}

function NavItem({ icon, label, active, onClick }: { icon: React.ReactNode; label: string; active: boolean; onClick: () => void }) {
	return (
		<button onClick={onClick} className={`nav-btn ${active ? "active" : ""}`}>
			{icon}
			<span>{label}</span>
			{active && <div className="ml-auto w-1 h-4 bg-y rounded-full" />}
		</button>
	);
}

function RecordingIndicator({ status, duration }: { status: string; duration: number }) {
	const isRec = status === "recording";
	return (
		<div className={`rounded-lg px-3 py-2 border ${isRec ? "border-red-700 bg-[#1a0000]" : "border-border bg-card"}`}>
			<div className="flex items-center gap-2">
				<div className={`w-2 h-2 rounded-full ${isRec ? "bg-red-500 rec-pulse" : "bg-text-dim"}`} />
				<span className={`text-xs font-bold ${isRec ? "text-red-400" : "text-text-dim"}`}>
					{isRec ? "REC" : "READY"}
				</span>
				{isRec && (
					<span className="text-xs text-red-400 ml-auto font-mono">{formatDur(duration)}</span>
				)}
			</div>
		</div>
	);
}

function QuickClipButtons({ onSave1Min, onSave5Min, active }: { onSave1Min: () => void; onSave5Min: () => void; active: boolean }) {
	return (
		<div className="space-y-1">
			<p className="label px-1">Quick Save</p>
			<button
				onClick={onSave1Min}
				disabled={!active}
				className="w-full text-xs font-bold py-1.5 rounded border border-y text-y
                           hover:bg-y hover:text-black transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
			>
				⚡ Last 1 Min
			</button>
			<button
				onClick={onSave5Min}
				disabled={!active}
				className="w-full text-xs font-bold py-1.5 rounded border border-[#444] text-text-mid
                           hover:border-y hover:text-y transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
			>
				⚡ Last 5 Min
			</button>
		</div>
	);
}

function formatDur(s: number) {
	const m = Math.floor(s / 60);
	const sec = s % 60;
	return `${String(m).padStart(2, "0")}:${String(sec).padStart(2, "0")}`;
}
