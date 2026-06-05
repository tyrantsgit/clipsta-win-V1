import { useCallback, useEffect, useRef, useState } from "react";
import { Monitor, Gamepad2, RefreshCw, Play, Square, Clock, Zap, ChevronDown } from "lucide-react";
import type { ScreenSource, AppSettings } from "../../types";
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

export default function CapturePage({ recorder, settings }: Props) {
	const [sources, setSources] = useState<ScreenSource[]>([]);
	const [selectedSource, setSelectedSource] = useState<ScreenSource | null>(null);
	const [loadingSources, setLoadingSources] = useState(false);
	const [showPicker, setShowPicker] = useState(false);
	const previewRef = useRef<HTMLVideoElement>(null);
	const streamRef = useRef<MediaStream | null>(null);

	const { status, duration, error } = recorder.state;
	const isRecording = status === "recording";

	const loadSources = useCallback(async () => {
		setLoadingSources(true);
		try {
			if (window.clipsta) {
				const srcs = await window.clipsta.getSources();
				setSources(srcs);
				// Auto-select first fullscreen or game
				const auto = srcs.find((s) => s.name.includes("Screen") || s.id.startsWith("screen"))
					?? srcs[0] ?? null;
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

	// Live preview of selected source
	useEffect(() => {
		if (!selectedSource || isRecording) return;
		let active = true;
		(async () => {
			try {
				if (streamRef.current) { streamRef.current.getTracks().forEach((t) => t.stop()); }
				await window.clipsta?.setPendingSource(selectedSource.id);
				const stream = await navigator.mediaDevices.getDisplayMedia({
					video: { frameRate: 15 },
				});
				// Remove audio track if present – preview doesn't need it
				stream.getAudioTracks().forEach((t) => { stream.removeTrack(t); t.stop(); });
				if (!active) { stream.getTracks().forEach((t) => t.stop()); return; }
				streamRef.current = stream;
				if (previewRef.current) {
					previewRef.current.srcObject = stream;
					previewRef.current.play();
				}
			} catch {}
		})();
		return () => { active = false; streamRef.current?.getTracks().forEach((t) => t.stop()); };
	}, [selectedSource, isRecording]);

	const handleToggle = async () => {
		await recorder.toggleRecording(selectedSource?.id ?? null);
	};

	// Separate game sources from screen sources
	const gameSources = sources.filter((s) => !s.id.startsWith("screen"));
	const screenSources = sources.filter((s) => s.id.startsWith("screen"));

	return (
		<div className="h-full overflow-y-auto p-6 space-y-5">
			<div className="flex items-center justify-between">
				<div>
					<h1 className="text-2xl font-black text-white">Capture</h1>
					<p className="text-text-dim text-sm mt-0.5">Record gameplay or any screen</p>
				</div>
				<div className="flex items-center gap-2">
					{settings.gameDetect && (
						<span className="tag bg-[#1c1c00] border border-[#3a3a00] text-y">
							<Gamepad2 size={10} className="inline mr-1" />AUTO DETECT
						</span>
					)}
				</div>
			</div>

			<div className="grid grid-cols-3 gap-4">
				{/* ── Preview ── */}
				<div className="col-span-2 space-y-3">
					<div className="relative bg-black rounded-xl overflow-hidden aspect-video border border-border">
						{isRecording ? (
							<>
								<video ref={previewRef} className="w-full h-full object-cover opacity-60" muted />
								<div className="absolute inset-0 flex items-center justify-center">
									<div className="text-center">
										<div className="flex items-center gap-2 bg-red-600 rounded-full px-4 py-2 mx-auto w-fit mb-2">
											<div className="w-2 h-2 bg-white rounded-full rec-pulse" />
											<span className="text-white font-bold text-sm">RECORDING</span>
										</div>
										<span className="text-white text-3xl font-mono font-bold">
											{formatDur(duration)}
										</span>
									</div>
								</div>
							</>
						) : selectedSource ? (
							<video ref={previewRef} className="w-full h-full object-cover" muted />
						) : (
							<div className="w-full h-full flex items-center justify-center text-text-dim">
								<div className="text-center">
									<Monitor size={48} className="mx-auto mb-3 opacity-30" />
									<p className="text-sm">Select a source to preview</p>
								</div>
							</div>
						)}

						{/* Source badge */}
						{selectedSource && !isRecording && (
							<div className="absolute top-3 left-3 bg-black/70 rounded px-2 py-1 text-xs text-white">
								{selectedSource.name}
							</div>
						)}
					</div>

					{/* Controls */}
					<div className="flex items-center gap-3">
						<button
							onClick={handleToggle}
							className={`flex-1 flex items-center justify-center gap-3 py-4 rounded-xl font-black text-lg transition-all
								${isRecording
									? "bg-red-600 hover:bg-red-500 text-white glow-red"
									: "bg-y hover:bg-yd text-black glow-y"
								}`}
						>
							{isRecording ? <Square size={22} fill="white" /> : <Play size={22} fill="black" />}
							{isRecording ? "STOP RECORDING" : "START RECORDING"}
						</button>
					</div>

					{/* Clip save buttons */}
					<div className="grid grid-cols-2 gap-3">
						<ClipBtn
							icon={<Zap size={15} />}
							label="Save Last 1 Min"
							hotkey={settings.hotkeyClip1Min}
							disabled={!isRecording}
							onClick={() => recorder.saveClip(60)}
						/>
						<ClipBtn
							icon={<Clock size={15} />}
							label="Save Last 5 Min"
							hotkey={settings.hotkeyClip5Min}
							disabled={!isRecording}
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

					{/* Selected */}
					{selectedSource && (
						<div className="card p-3 border-y border-opacity-50">
							<img src={selectedSource.thumbnail} className="w-full rounded aspect-video object-cover mb-2" alt="" />
							<p className="text-white text-xs font-semibold truncate">{selectedSource.name}</p>
							<p className="text-text-dim text-[10px]">Active source</p>
						</div>
					)}

					{/* Screens */}
					{screenSources.length > 0 && (
						<SourceGroup label="Displays" sources={screenSources} selected={selectedSource} onSelect={setSelectedSource} />
					)}

					{/* Windows / Games */}
					{gameSources.length > 0 && (
						<SourceGroup label="Windows & Games" sources={gameSources} selected={selectedSource} onSelect={setSelectedSource} />
					)}

					{/* Settings summary */}
					<div className="card p-3 space-y-2 mt-2">
						<p className="label">Recording Config</p>
						<Row label="Resolution" val={settings.resolution} />
						<Row label="FPS" val={String(settings.fps)} />
						<Row label="Encoder" val={settings.encoder} />
						<Row label="Bitrate" val={`${settings.bitrate} kbps`} />
					</div>
				</div>
			</div>
		</div>
	);
}

function SourceGroup({ label, sources, selected, onSelect }: {
	label: string;
	sources: ScreenSource[];
	selected: ScreenSource | null;
	onSelect: (s: ScreenSource) => void;
}) {
	const [open, setOpen] = useState(true);
	return (
		<div>
			<button onClick={() => setOpen((o) => !o)} className="flex items-center gap-1 label mb-1.5 w-full hover:text-white transition-colors">
				{label}
				<ChevronDown size={10} className={`ml-auto transition-transform ${open ? "" : "-rotate-90"}`} />
			</button>
			{open && (
				<div className="space-y-1 max-h-40 overflow-y-auto">
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
								<Monitor size={13} className="flex-shrink-0" />
							)}
							<span className="truncate">{s.name}</span>
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
