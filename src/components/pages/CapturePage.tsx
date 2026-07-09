import { useCallback, useEffect, useRef, useState } from "react";
import { Monitor, Gamepad2, RefreshCw, Play, Square, Clock, Zap, ChevronDown } from "lucide-react";
import type { ScreenSource, WgcSource, AppSettings } from "../../types";
import type { RecorderState } from "../../hooks/useRecorder";

interface Props {
	recorder: {
		state: RecorderState;
		startRecording: (sourceId?: string | null) => Promise<void>;
		stopRecording: () => void;
		toggleRecording: (sourceId?: string | null) => Promise<void>;
		saveClip: (seconds: number) => Promise<string | null>;
		saveFullRecording: () => Promise<string | null>;
		setSourceId: (id: string | null) => void;
	};
	settings: AppSettings;
}

/** Unified source model used in the UI */
interface UnifiedSource {
	id: string;
	name: string;
	thumbnail?: string;
	appIcon?: string | null;
	source_type: "monitor" | "window";
	width?: number;
	height?: number;
}

export default function CapturePage({ recorder, settings }: Props) {
	const [sources, setSources] = useState<UnifiedSource[]>([]);
	const [selectedSource, setSelectedSource] = useState<UnifiedSource | null>(null);
	const [loadingSources, setLoadingSources] = useState(false);
	const [useWgc, setUseWgc] = useState(false);
	const previewRef = useRef<HTMLVideoElement>(null);
	const streamRef = useRef<MediaStream | null>(null);

	const { status, duration, error } = recorder.state;
	const isRecording = status === "recording";
	const isSaving = status === "saving";

	const loadSources = useCallback(async () => {
		setLoadingSources(true);
		try {
			if (!window.clipsta) return;

			// Try WGC sources first (richer, more accurate)
			let wgcSrcs: WgcSource[] = [];
			if (window.clipsta.getWgcSources) {
				try { wgcSrcs = await window.clipsta.getWgcSources(); } catch { /* ignore */ }
			}

			if (wgcSrcs.length > 0) {
				setUseWgc(true);
				const unified: UnifiedSource[] = wgcSrcs.map((s) => ({
					id: s.id,
					name: s.name,
					source_type: s.source_type,
					width: s.width,
					height: s.height,
				}));
				setSources(unified);
				// Auto-select primary monitor
				const auto = unified.find((s) => s.source_type === "monitor") ?? unified[0] ?? null;
				if (!selectedSource) setSelectedSource(auto);
			} else {
				// Fallback to Electron desktopCapturer
				setUseWgc(false);
				const srcs: ScreenSource[] = await window.clipsta.getSources();
				const unified: UnifiedSource[] = srcs.map((s) => ({
					id: s.id,
					name: s.name,
					thumbnail: s.thumbnail,
					appIcon: s.appIcon,
					source_type: s.id.startsWith("screen") ? "monitor" : "window",
				}));
				setSources(unified);
				const auto = unified.find((s) => s.source_type === "monitor") ?? unified[0] ?? null;
				if (!selectedSource) setSelectedSource(auto);
			}
		} finally {
			setLoadingSources(false);
		}
	}, [selectedSource]);

	useEffect(() => { loadSources(); }, []);

	// Sync selected source to recorder for hotkey use
	useEffect(() => {
		recorder.setSourceId(selectedSource?.id ?? null);
	}, [selectedSource]);

	// Live preview (only in non-WGC mode, since WGC uses a separate capture process)
	useEffect(() => {
		if (useWgc || !selectedSource || isRecording) return;
		let active = true;
		(async () => {
			try {
				if (streamRef.current) { streamRef.current.getTracks().forEach((t) => t.stop()); }
				await window.clipsta?.setPendingSource(selectedSource.id);
				const stream = await navigator.mediaDevices.getDisplayMedia({ video: { frameRate: 15 } });
				stream.getAudioTracks().forEach((t) => { stream.removeTrack(t); t.stop(); });
				if (!active) { stream.getTracks().forEach((t) => t.stop()); return; }
				streamRef.current = stream;
				if (previewRef.current) { previewRef.current.srcObject = stream; previewRef.current.play().catch(() => {}); }
			} catch {}
		})();
		return () => {
			active = false;
			streamRef.current?.getTracks().forEach((t) => t.stop());
		};
	}, [selectedSource, isRecording, useWgc]);

	const handleToggle = async () => {
		await recorder.toggleRecording(selectedSource?.id ?? null);
	};

	const monitorSources = sources.filter((s) => s.source_type === "monitor");
	const windowSources = sources.filter((s) => s.source_type === "window");

	return (
		<div className="h-full overflow-y-auto p-6 space-y-5">
			<div className="flex items-center justify-between">
				<div>
					<h1 className="text-2xl font-black text-white">Capture</h1>
					<p className="text-text-dim text-sm mt-0.5">
						{useWgc ? "WGC hardware capture active" : "Record gameplay or any screen"}
					</p>
				</div>
				<div className="flex items-center gap-2">
					{useWgc && (
						<span className="tag bg-[#001c00] border border-[#003a00] text-green-400">
							<Monitor size={10} className="inline mr-1" />WGC
						</span>
					)}
					{settings.gameDetect && (
						<span className="tag bg-[#1c1c00] border border-[#3a3a00] text-y">
							<Gamepad2 size={10} className="inline mr-1" />AUTO DETECT
						</span>
					)}
				</div>
			</div>

			<div className="grid grid-cols-3 gap-4">
				{/* ── Preview / Recording indicator ── */}
				<div className="col-span-2 space-y-3">
					<div className="relative bg-black rounded-xl overflow-hidden aspect-video border border-border">
						{isRecording ? (
							<div className="w-full h-full flex items-center justify-center">
								<div className="text-center">
									<div className="flex items-center gap-2 bg-red-600 rounded-full px-4 py-2 mx-auto w-fit mb-2">
										<div className="w-2 h-2 bg-white rounded-full rec-pulse" />
										<span className="text-white font-bold text-sm">
											{useWgc ? "RECORDING (WGC)" : "RECORDING"}
										</span>
									</div>
									<span className="text-white text-3xl font-mono font-bold">
										{formatDur(duration)}
									</span>
									{selectedSource && (
										<p className="text-text-dim text-sm mt-2">{selectedSource.name}</p>
									)}
								</div>
							</div>
						) : selectedSource ? (
							<>
								{!useWgc && <video ref={previewRef} className="w-full h-full object-cover" muted />}
								{useWgc && (
									<div className="w-full h-full flex items-center justify-center">
										<div className="text-center">
											<Monitor size={48} className="mx-auto mb-3 opacity-40 text-green-400" />
											<p className="text-text-dim text-sm">
												{selectedSource.name}
											</p>
											{selectedSource.width && (
												<p className="text-text-dim text-xs mt-1">
													{selectedSource.width}×{selectedSource.height} · WGC
												</p>
											)}
										</div>
									</div>
								)}
								{/* Source badge */}
								{!useWgc && (
									<div className="absolute top-3 left-3 bg-black/70 rounded px-2 py-1 text-xs text-white">
										{selectedSource.name}
									</div>
								)}
							</>
						) : (
							<div className="w-full h-full flex items-center justify-center text-text-dim">
								<div className="text-center">
									<Monitor size={48} className="mx-auto mb-3 opacity-30" />
									<p className="text-sm">Select a source to capture</p>
								</div>
							</div>
						)}
					</div>

					{/* Controls — Replay buffer is always-on (like Game Bar) */}
					<div className="flex items-center gap-3">
						{isRecording ? (
							<div className="flex-1 flex items-center gap-3">
								<div className="flex-1 flex items-center justify-center gap-3 py-3 rounded-xl bg-[#0a1a0a] border border-green-800">
									<div className="w-2.5 h-2.5 bg-green-500 rounded-full rec-pulse" />
									<span className="text-green-400 font-bold text-sm">REPLAY BUFFER ACTIVE</span>
									<span className="text-green-600 text-xs font-mono">{formatDur(duration)}</span>
								</div>
								<button
									onClick={handleToggle}
									className="px-4 py-3 rounded-xl border border-red-800 bg-[#1a0a0a] text-red-400 hover:bg-red-900/30 transition-colors text-xs font-bold"
									title="Stop Replay Buffer"
								>
									<Square size={14} />
								</button>
							</div>
						) : (
							<button
								onClick={handleToggle}
								className="flex-1 flex items-center justify-center gap-3 py-4 rounded-xl font-black text-lg bg-y hover:bg-yd text-black glow-y transition-all"
							>
								<Play size={22} fill="black" />
								START REPLAY BUFFER
							</button>
						)}
					</div>

					{/* Clip save buttons — primary action */}
					<div className="grid grid-cols-2 gap-3">
					<ClipBtn
						icon={<Zap size={15} />}
						label="Save Last 1 Min"
						hotkey={settings.hotkeyClip1Min}
						disabled={isSaving}
						onClick={() => recorder.saveClip(60)}
					/>
					<ClipBtn
						icon={<Clock size={15} />}
						label="Save Last 5 Min"
						hotkey={settings.hotkeyClip5Min}
						disabled={isSaving}
						onClick={() => recorder.saveClip(300)}
					/>
					</div>

					{error && (
						<div className="bg-red-900/30 border border-red-700 rounded-lg px-4 py-3 text-red-300 text-sm">
							⚠ {error}
						</div>
					)}
				</div>

				{/* ── Source picker ── */}
				<div className="space-y-3">
					<div className="flex items-center justify-between">
						<h3 className="text-sm font-semibold text-white">Source</h3>
						<button onClick={loadSources} className="text-text-dim hover:text-y transition-colors" title="Refresh">
							<RefreshCw size={13} className={loadingSources ? "animate-spin" : ""} />
						</button>
					</div>

					{/* Selected source card */}
					{selectedSource && (
						<div className="card p-3 border-y border-opacity-50">
							{selectedSource.thumbnail ? (
								<img src={selectedSource.thumbnail} className="w-full rounded aspect-video object-cover mb-2" alt="" />
							) : (
								<div className="w-full rounded aspect-video bg-surface flex items-center justify-center mb-2">
									<Monitor size={24} className={selectedSource.source_type === "monitor" ? "text-green-400" : "text-text-dim"} />
								</div>
							)}
							<p className="text-white text-xs font-semibold truncate">{selectedSource.name}</p>
							<p className="text-text-dim text-[10px]">Active source{useWgc ? " · WGC" : ""}</p>
						</div>
					)}

					{/* Displays */}
					{monitorSources.length > 0 && (
						<SourceGroup
							label="Displays"
							sources={monitorSources}
							selected={selectedSource}
							onSelect={setSelectedSource}
						/>
					)}

					{/* Windows & Games */}
					{windowSources.length > 0 && (
						<SourceGroup
							label="Windows & Games"
							sources={windowSources}
							selected={selectedSource}
							onSelect={setSelectedSource}
						/>
					)}

					{/* Settings summary */}
					<div className="card p-3 space-y-2 mt-2">
						<p className="label">Recording Config</p>
						<Row label="Resolution" val={settings.resolution} />
						<Row label="FPS" val={String(settings.fps)} />
						<Row label="Encoder" val={settings.encoder} />
						<Row label="Bitrate" val={`${settings.bitrate} kbps`} />
						<Row label="Capture" val={useWgc ? "WGC" : "MediaRecorder"} />
					</div>
				</div>
			</div>
		</div>
	);
}

function SourceGroup({ label, sources, selected, onSelect }: {
	label: string;
	sources: UnifiedSource[];
	selected: UnifiedSource | null;
	onSelect: (s: UnifiedSource) => void;
}) {
	const [open, setOpen] = useState(true);
	return (
		<div>
			<button
				onClick={() => setOpen((o) => !o)}
				className="flex items-center gap-1 label mb-1.5 w-full hover:text-white transition-colors"
			>
				{label}
				<ChevronDown size={10} className={`ml-auto transition-transform ${open ? "" : "-rotate-90"}`} />
			</button>
			{open && (
				<div className="space-y-1 max-h-48 overflow-y-auto">
					{sources.map((s) => (
						<button
							key={s.id}
							onClick={() => onSelect(s)}
							className={`w-full text-left flex items-center gap-2 p-2 rounded-lg border transition-all text-xs
								${selected?.id === s.id
									? "border-y bg-[#1c1c00] text-y"
									: "border-border hover:border-muted text-text-mid hover:text-white"
								}`}
						>
							{s.appIcon ? (
								<img src={s.appIcon} className="w-4 h-4 rounded flex-shrink-0" alt="" />
							) : (
								<Monitor size={13} className={`flex-shrink-0 ${s.source_type === "monitor" ? "text-green-400" : ""}`} />
							)}
							<span className="truncate">{s.name}</span>
							{s.width && (
								<span className="ml-auto text-text-dim text-[10px] flex-shrink-0">
									{s.width}×{s.height}
								</span>
							)}
						</button>
					))}
				</div>
			)}
		</div>
	);
}

function ClipBtn({ icon, label, hotkey, disabled, onClick }: {
	icon: React.ReactNode; label: string; hotkey: string; disabled: boolean; onClick: () => void;
}) {
	return (
		<button
			onClick={onClick}
			disabled={disabled}
			className="flex items-center gap-2 p-3 rounded-xl border border-border
                       hover:border-y hover:bg-[#1c1c00] transition-all
                       disabled:opacity-30 disabled:cursor-not-allowed text-left"
		>
			<span className="text-y">{icon}</span>
			<div>
				<p className="text-white text-sm font-semibold">{label}</p>
				<p className="text-text-dim text-xs">{hotkey}</p>
			</div>
		</button>
	);
}

function Row({ label, val }: { label: string; val: string }) {
	return (
		<div className="flex justify-between text-xs">
			<span className="text-text-dim">{label}</span>
			<span className="text-white font-medium">{val}</span>
		</div>
	);
}

function formatDur(s: number) {
	return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
}
