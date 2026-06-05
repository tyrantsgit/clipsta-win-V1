import { useCallback, useEffect, useRef, useState } from "react";
import {
	Scissors, Download, RotateCcw, Volume2, VolumeX, FileUp,
	Play, Pause, SkipBack, SkipForward, Crop, Loader2, Trash2, Eraser, FolderOpen,
} from "lucide-react";
import type { AppSettings, ExportOpts } from "../../types";

function toFileUrl(p: string): string {
	if (p.startsWith("file://")) return p;
	const normalized = p.replace(/\\/g, "/");
	const cleaned = normalized.startsWith("/") ? normalized.slice(1) : normalized;
	return `file:///${cleaned.replace(/#/g, "%23").replace(/\?/g, "%3F")}`;
}

interface Props {
	initialFile: string | null;
	settings: AppSettings;
}

interface CutSeg { start: number; end: number }

export default function EditorPage({ initialFile, settings }: Props) {
	const [filePath, setFilePath] = useState<string | null>(initialFile);
	const [playing, setPlaying] = useState(false);
	const [currentTime, setCurrentTime] = useState(0);
	const [duration, setDuration] = useState(0);
	const [volume, setVolume] = useState(1);
	const [muted, setMuted] = useState(false);
	const [trimIn, setTrimIn] = useState(0);
	const [trimOut, setTrimOut] = useState(0);
	const [cuts, setCuts] = useState<CutSeg[]>([]);
	const [exporting, setExporting] = useState(false);
	const [exportDone, setExportDone] = useState<string | null>(null);
	const [exportError, setExportError] = useState<string | null>(null);
	const videoRef = useRef<HTMLVideoElement>(null);
	const [timelineScale, setTimelineScale] = useState(1);
	const [dragOver, setDragOver] = useState(false);

	const [expFormat, setExpFormat] = useState("mp4");
	const [expResolution, setExpResolution] = useState(settings.resolution);
	const [expAspect, setExpAspect] = useState(settings.aspectRatio);

	const loadFile = useCallback((path: string) => {
		setFilePath(path);
		setTrimIn(0);
		setTrimOut(0);
		setCurrentTime(0);
		setDuration(0);
		setCuts([]);
		setExportDone(null);
		setExportError(null);
		setPlaying(false);
	}, []);

	useEffect(() => {
		if (initialFile && initialFile !== filePath) loadFile(initialFile);
	}, [initialFile, filePath, loadFile]);

	const onLoadedMetadata = () => {
		const v = videoRef.current;
		if (!v || !isFinite(v.duration)) return;
		setDuration(v.duration);
		setTrimOut(v.duration);
	};

	const onTimeUpdate = () => {
		const v = videoRef.current;
		if (!v) return;
		setCurrentTime(v.currentTime);
		if (v.currentTime >= trimOut) {
			v.currentTime = trimIn;
			if (!playing) v.pause();
		}
	};

	const seek = (t: number) => {
		if (videoRef.current) videoRef.current.currentTime = t;
		setCurrentTime(t);
	};

	const getTimeFromEvent = (clientX: number, parentEl: HTMLElement) => {
		const rect = parentEl.getBoundingClientRect();
		return ((clientX - rect.left) / rect.width) * duration;
	};

	// Drag-scrub state
	const scrubRef = useRef(false);

	const handleTimelineMouseDown = (e: React.MouseEvent) => {
		const parent = e.currentTarget;
		const t = Math.max(0, Math.min(duration, getTimeFromEvent(e.clientX, parent)));
		seek(t);
		scrubRef.current = true;
		const move = (me: MouseEvent) => {
			if (!scrubRef.current) return;
			const t2 = Math.max(0, Math.min(duration, getTimeFromEvent(me.clientX, parent)));
			seek(t2);
		};
		const up = () => {
			scrubRef.current = false;
			window.removeEventListener("mousemove", move);
			window.removeEventListener("mouseup", up);
		};
		window.addEventListener("mousemove", move);
		window.addEventListener("mouseup", up);
	};

	const handleTimelineContext = (e: React.MouseEvent) => {
		e.preventDefault();
		const parent = e.currentTarget as HTMLElement;
		const t = Math.max(0, Math.min(duration, getTimeFromEvent(e.clientX, parent)));
		// Add a cut segment of 2s around the clicked point
		const half = 1;
		const start = Math.max(0, t - half);
		const end = Math.min(duration, t + half);
		setCuts((prev) => {
			const merged = [...prev, { start, end }].sort((a, b) => a.start - b.start);
			return merged;
		});
	};

	const togglePlay = () => {
		const v = videoRef.current;
		if (!v) return;
		if (playing) { v.pause(); setPlaying(false); }
		else {
			if (v.currentTime >= trimOut || v.currentTime < trimIn) v.currentTime = trimIn;
			v.play(); setPlaying(true);
		}
	};

	const setVol = (val: number) => {
		setVolume(val);
		if (videoRef.current) videoRef.current.volume = val;
	};

	const removeSegment = () => {
		const segLen = trimOut - trimIn;
		if (segLen < 0.5) return;
		setCuts((prev) => [...prev, { start: trimIn, end: trimOut }].sort((a, b) => a.start - b.start));
	};

	const removeCut = (idx: number) => {
		setCuts((prev) => prev.filter((_, i) => i !== idx));
	};

	const handleExport = async () => {
		if (!filePath) return;
		const baseName = filePath.replace(/^.*[\\/]/, "").replace(/\.[^.]+$/, "");
		const ext = expFormat === "webm" ? "webm" : expFormat === "mkv" ? "mkv" : expFormat === "mov" ? "mov" : "mp4";
		const savePath = await window.clipsta?.browseSaveExport(`${baseName}_export.${ext}`);
		if (!savePath) return;
		setExporting(true);
		setExportDone(null);
		setExportError(null);
		try {
			const opts: ExportOpts = {
				format: expFormat,
				resolution: expResolution,
				aspectRatio: expAspect,
				trimStart: trimIn > 0 ? trimIn : undefined,
				trimEnd: trimOut < duration ? trimOut : undefined,
				cuts: cuts.length > 0 ? cuts : undefined,
			};
			const out = await window.clipsta?.exportRecording(filePath, savePath, opts);
			setExportDone(out ?? null);
		} catch (e: any) {
			setExportError(e?.message ?? String(e));
		} finally {
			setExporting(false);
		}
	};

	const pct = (t: number) => duration > 0 ? (t / duration) * 100 : 0;

	// ── Keyboard shortcuts ────────────────────────────────────────────────
	useEffect(() => {
		const onKey = (e: KeyboardEvent) => {
			if (e.target instanceof HTMLInputElement || e.target instanceof HTMLSelectElement || e.target instanceof HTMLTextAreaElement) return;
			const t = videoRef.current?.currentTime ?? currentTime;
			switch (e.code) {
				case "Space": e.preventDefault(); togglePlay(); break;
				case "KeyI": setTrimIn(t); if (videoRef.current) videoRef.current.currentTime = t; break;
				case "KeyO": setTrimOut(t); break;
				case "KeyX": removeSegment(); break;
			}
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	});

	// ── Drag-and-drop handlers (counter-based to avoid child-element flicker) ──
	const dragCounterRef = useRef(0);

	const getDroppedPath = (dt: DataTransfer): string | null => {
		const f = dt.files[0];
		if (!f) return null;
		if ((f as any).path) return (f as any).path;
		return null;
	};

	const handleDrop = (e: React.DragEvent) => {
		e.preventDefault();
		dragCounterRef.current = 0;
		setDragOver(false);
		const path = getDroppedPath(e.dataTransfer);
		if (path) loadFile(path);
	};

	const handleDragEnter = (e: React.DragEvent) => {
		e.preventDefault();
		dragCounterRef.current++;
		setDragOver(true);
	};

	const handleDragOver = (e: React.DragEvent) => {
		e.preventDefault();
	};

	const handleDragLeave = (e: React.DragEvent) => {
		e.preventDefault();
		dragCounterRef.current--;
		if (dragCounterRef.current <= 0) {
			dragCounterRef.current = 0;
			setDragOver(false);
		}
	};

	const handleBrowse = async () => {
		const path = await window.clipsta?.browseFile();
		if (path) loadFile(path);
	};

	// ── Aspect ratio CSS ───────────────────────────────────────────────────
			const aspectRatioValue = {
				"16:9": 16 / 9, "9:16": 9 / 16,
				"1:1": 1, "4:3": 4 / 3, "21:9": 21 / 9,
			}[expAspect] ?? 16 / 9;

	// ── Time ruler ticks ───────────────────────────────────────────────────
	const tickInterval = Math.max(1, Math.floor(10 / timelineScale));
	const ticks: number[] = [];
	if (duration > 0 && isFinite(duration)) {
		const maxTicks = 10000;
		for (let t = 0, count = 0; t <= duration && count < maxTicks; t += tickInterval, count++) ticks.push(t);
	}

	if (!filePath) {
		return (
			<div
				className={`h-full flex items-center justify-center flex-col gap-4 text-center p-8 transition-colors ${dragOver ? "bg-[#1c1c00]" : ""}`}
				onDrop={handleDrop}
				onDragEnter={handleDragEnter}
				onDragOver={handleDragOver}
				onDragLeave={handleDragLeave}
			>
				<div className={`rounded-2xl border-2 border-dashed p-12 flex flex-col items-center gap-4 transition-all duration-200 ${dragOver ? "border-y bg-[#2a2a00] scale-105 shadow-[0_0_40px_rgba(212,240,0,0.15)]" : "border-border hover:border-text-dim"}`}>
					<Scissors size={48} className={`transition-colors ${dragOver ? "text-y" : "text-text-dim opacity-30"}`} />
					<div>
						<p className={`text-xl font-bold transition-colors ${dragOver ? "text-y" : "text-white"}`}>
							{dragOver ? "Drop to start editing" : "Drop a video here"}
						</p>
						<p className="text-text-dim text-sm mt-1">or open from the Library to start editing</p>
					</div>
					<button onClick={handleBrowse} className="btn-y mt-2">
						<FolderOpen size={14} /> Browse Files
					</button>
				</div>
			</div>
		);
	}

	return (
		<div
			className={`h-full flex overflow-hidden transition-colors ${dragOver ? "bg-[#1c1c00]" : ""}`}
			onDrop={handleDrop}
			onDragEnter={handleDragEnter}
			onDragOver={handleDragOver}
			onDragLeave={handleDragLeave}
		>
			{/* Drop overlay */}
			{dragOver && (
				<div className="absolute inset-0 z-50 flex items-center justify-center bg-black/70 pointer-events-none">
					<div className="rounded-2xl border-2 border-dashed border-y p-10 text-center bg-[#1c1c00]/80 backdrop-blur-sm shadow-[0_0_60px_rgba(212,240,0,0.1)]">
						<FileUp size={40} className="mx-auto mb-3 text-y" />
						<p className="text-y text-lg font-bold">Drop video to load</p>
						<p className="text-text-dim text-sm mt-1">Supports MP4, WebM, MKV, MOV</p>
					</div>
				</div>
			)}
			{/* ── Main editor area ── */}
			<div className="flex-1 flex flex-col overflow-hidden">
				{/* Video preview */}
				<div className="flex-1 bg-black flex items-center justify-center overflow-hidden min-h-0 p-4 relative group/preview">
					<div
						className="relative bg-black flex items-center justify-center overflow-hidden rounded-lg border border-border"
						style={{ aspectRatio: `${aspectRatioValue}`, maxWidth: "100%", maxHeight: "100%", width: "100%", height: "auto" }}
					>
						<video
							ref={videoRef}
							src={toFileUrl(filePath)}
							className="max-w-full max-h-full"
							style={{ objectFit: "cover" }}
							onLoadedMetadata={onLoadedMetadata}
							onTimeUpdate={onTimeUpdate}
							onEnded={() => setPlaying(false)}
							volume={muted ? 0 : volume}
						/>
					</div>
					{/* Replace overlay */}
					<div className="absolute top-2 right-2 flex gap-2 opacity-0 group-hover/preview:opacity-100 transition-opacity">
						<button onClick={handleBrowse} className="btn-ghost text-xs" title="Open file">
							<FolderOpen size={12} /> Open
						</button>
					</div>
				</div>

				{/* ── Timeline ── */}
				<div className="bg-[#0d0d0d] border-t border-border px-4 py-3 space-y-2 flex-shrink-0">
					{duration > 0 && (
						<div className="relative h-4 overflow-hidden select-none" style={{ fontSize: 0 }}>
							{ticks.map((t) => (
								<div
									key={t}
									className="absolute top-0 flex flex-col items-start"
									style={{ left: `${pct(t)}%`, transform: "translateX(-50%)" }}
								>
									<div className="h-2 w-px bg-muted" />
									<span className="text-[9px] text-text-dim font-mono mt-0.5" style={{ fontSize: 9 }}>
										{Math.floor(t / 60)}:{String(Math.floor(t % 60)).padStart(2, "0")}
									</span>
								</div>
							))}
						</div>
					)}

					<div
						className="relative h-12 bg-muted rounded-lg overflow-hidden cursor-pointer group select-none"
						onMouseDown={handleTimelineMouseDown}
						onContextMenu={handleTimelineContext}
					>
						{ticks.map((t) => (
							<div
								key={t}
								className="absolute top-0 h-full w-px bg-black/30"
								style={{ left: `${pct(t)}%` }}
							/>
						))}

						<div
							className="absolute top-0 h-full bg-[#D4F00022]"
							style={{ left: `${pct(trimIn)}%`, width: `${pct(trimOut) - pct(trimIn)}%` }}
						/>

						{cuts.map((c, i) => (
							<div
								key={i}
								className="absolute top-0 h-full bg-red-600/50 z-[5] flex items-center justify-center"
								style={{ left: `${pct(c.start)}%`, width: `${pct(c.end) - pct(c.start)}%` }}
							>
								<div className="w-full h-px bg-red-400/60" />
							</div>
						))}

						<div
							className="absolute top-0 w-0.5 h-full bg-white z-10 shadow-lg"
							style={{ left: `${pct(currentTime)}%` }}
						>
							<div className="w-3 h-3 bg-white rounded-full -ml-[5px] -mt-[1px]" />
						</div>

						<div
							className="absolute top-0 h-full w-4 -translate-x-1/2 flex items-center cursor-ew-resize group/handle z-20"
							style={{ left: `${pct(trimIn)}%` }}
							onMouseDown={(e) => {
								e.stopPropagation();
								const parent = e.currentTarget.parentElement!;
								const move = (me: MouseEvent) => {
									const rect = parent.getBoundingClientRect();
									const t = ((me.clientX - rect.left) / rect.width) * duration;
									setTrimIn(Math.max(0, Math.min(trimOut - 0.5, t)));
								};
								const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); };
								window.addEventListener("mousemove", move);
								window.addEventListener("mouseup", up);
							}}
						>
							<div className="w-1 h-full bg-y rounded-l group-hover/handle:shadow-[0_0_8px_#D4F000]" />
							<div className="absolute top-0 bg-y text-black text-[8px] font-black px-1 py-0.5 rounded-br flex items-center gap-0.5">
								<span className="text-[6px]">▶</span> IN
							</div>
						</div>

						<div
							className="absolute top-0 h-full w-4 translate-x-1/2 right-0 flex items-center cursor-ew-resize group/handle z-20"
							style={{ left: `${pct(trimOut)}%` }}
							onMouseDown={(e) => {
								e.stopPropagation();
								const parent = e.currentTarget.parentElement!;
								const move = (me: MouseEvent) => {
									const rect = parent.getBoundingClientRect();
									const t = ((me.clientX - rect.left) / rect.width) * duration;
									setTrimOut(Math.min(duration, Math.max(trimIn + 0.5, t)));
								};
								const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); };
								window.addEventListener("mousemove", move);
								window.addEventListener("mouseup", up);
							}}
						>
							<div className="w-1 h-full bg-y rounded-r group-hover/handle:shadow-[0_0_8px_#D4F000]" />
							<div className="absolute top-0 right-0 bg-y text-black text-[8px] font-black px-1 py-0.5 rounded-bl flex items-center gap-0.5">
								OUT <span className="text-[6px]">◀</span>
							</div>
						</div>
					</div>

					<div className="flex items-center justify-between gap-4">
						<div className="flex items-center gap-3 text-xs text-text-dim font-mono">
							<span className="flex items-center gap-1">
								<span className="text-y font-bold text-[10px]">IN</span>
								{formatTime(trimIn)}
							</span>
							<span className="text-white font-bold">{formatTime(currentTime)}</span>
							<span className="flex items-center gap-1">
								<span className="text-y font-bold text-[10px]">OUT</span>
								{formatTime(trimOut)}
							</span>
							<span className="text-text-mid text-[10px]">
								({formatTime(trimOut - trimIn)})
							</span>
						</div>

						<div className="flex items-center gap-2">
							<span className="text-[10px] text-text-dim">Zoom</span>
							<input
								type="range" min={1} max={5} step={0.5} value={timelineScale}
								onChange={(e) => setTimelineScale(Number(e.target.value))}
								className="w-16 accent-[#D4F000] no-drag"
							/>
						</div>
					</div>

					<div className="flex items-center justify-between">
						<div className="flex items-center gap-2">
							<button onClick={() => seek(trimIn)} className="text-text-mid hover:text-white transition-colors p-1" title="Jump to IN">
								<SkipBack size={16} />
							</button>
							<button
								onClick={togglePlay}
								className="w-8 h-8 rounded-full bg-y hover:bg-yd flex items-center justify-center transition-colors"
							>
								{playing
									? <Pause size={15} fill="black" className="text-black" />
									: <Play size={15} fill="black" className="text-black" />
								}
							</button>
							<button onClick={() => seek(trimOut)} className="text-text-mid hover:text-white transition-colors p-1" title="Jump to OUT">
								<SkipForward size={16} />
							</button>

							<div className="w-px h-5 bg-border mx-2" />

							<button
								onClick={() => { const t = videoRef.current?.currentTime ?? currentTime; setTrimIn(t); if (videoRef.current) videoRef.current.currentTime = t; }}
								className="text-[10px] font-bold px-2 py-1 rounded border border-border text-text-mid hover:border-y hover:text-y transition-colors"
								title="Set IN at current position [I]"
							>Set IN</button>
							<button
								onClick={() => { const t = videoRef.current?.currentTime ?? currentTime; setTrimOut(t); }}
								className="text-[10px] font-bold px-2 py-1 rounded border border-border text-text-mid hover:border-y hover:text-y transition-colors"
								title="Set OUT at current position [O]"
							>Set OUT</button>

							<div className="w-px h-5 bg-border mx-2" />

							<button
								onClick={removeSegment}
								disabled={trimOut - trimIn < 0.5}
								className="text-[10px] font-bold px-2 py-1 rounded border border-red-700 text-red-400 hover:bg-red-900/30 transition-colors disabled:opacity-30 disabled:cursor-not-allowed"
								title="Remove the IN–OUT segment [X]"
							><Eraser size={12} className="inline mr-1" />Remove IN–OUT</button>

							<div className="w-px h-5 bg-border mx-2" />

							<button onClick={() => setMuted((m) => !m)} className="text-text-mid hover:text-white transition-colors p-1">
								{muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
							</button>
							<input
								type="range" min={0} max={1} step={0.05} value={muted ? 0 : volume}
								onChange={(e) => setVol(Number(e.target.value))}
								className="w-16 accent-[#D4F000] no-drag"
							/>

							<div className="w-px h-5 bg-border mx-2" />

							<button
								onClick={() => { setTrimIn(0); setTrimOut(duration); }}
								className="text-text-dim hover:text-white transition-colors p-1"
								title="Reset trim"
							><RotateCcw size={14} /></button>
						</div>

						<div className="text-[9px] text-text-dim hidden md:block">
							<span className="inline-flex items-center gap-2">
								<span><kbd className="bg-muted px-1 rounded">I</kbd> IN</span>
								<span><kbd className="bg-muted px-1 rounded">O</kbd> OUT</span>
								<span><kbd className="bg-muted px-1 rounded">X</kbd> Cut</span>
								<span><kbd className="bg-muted px-1 rounded">Space</kbd> Play</span>
								<span><kbd className="bg-muted px-1 rounded">Right‑click</kbd> Quick Cut</span>
							</span>
						</div>
					</div>
				</div>
			</div>

			{/* ── Right panel ── */}
			<div className="w-64 flex-shrink-0 border-l border-border bg-[#0d0d0d] flex flex-col overflow-y-auto p-4 space-y-4">
				<h3 className="font-bold text-white text-sm">Export Options</h3>

				<Section title="Trim">
					<Row label="In" val={formatTime(trimIn)} />
					<Row label="Out" val={formatTime(trimOut)} />
					<Row label="Duration" val={formatTime(trimOut - trimIn)} />
				</Section>

				<Section title="Cuts / Removed Segments">
					{cuts.length === 0 ? (
						<p className="text-text-dim text-xs">Set IN and OUT, then click <b>Remove IN–OUT</b> to cut a segment.</p>
					) : (
						<div className="space-y-1 max-h-40 overflow-y-auto">
							{cuts.map((c, i) => (
								<div key={i} className="flex items-center justify-between bg-muted rounded px-2 py-1 text-xs">
									<span className="text-text-mid font-mono">
										{formatTime(c.start)} – {formatTime(c.end)}
										<span className="text-text-dim ml-1">({formatTime(c.end - c.start)})</span>
									</span>
									<button onClick={() => removeCut(i)} className="text-red-400 hover:text-red-300 flex-shrink-0 ml-1">
										<Trash2 size={12} />
									</button>
								</div>
							))}
						</div>
					)}
					{cuts.length > 0 && (
						<button onClick={() => setCuts([])} className="text-[10px] text-text-dim hover:text-white transition-colors flex items-center gap-1 mt-1">
							<RotateCcw size={10} /> Clear all cuts
						</button>
					)}
				</Section>

				<Section title="Format">
					<Select label="Container" value={expFormat} onChange={setExpFormat}
						options={["mp4", "webm", "mkv", "mov"]} />
				</Section>

				<Section title="Video">
					<Select label="Resolution" value={expResolution} onChange={setExpResolution}
						options={["480p", "720p", "1080p", "1440p", "4k"]} />
					<Select label="Aspect Ratio" value={expAspect} onChange={setExpAspect}
						options={["16:9", "9:16", "1:1", "4:3", "21:9"]} />
				</Section>

				<AspectPreview ratio={expAspect} />

				{exportDone && (
					<div className="bg-[#0a1a00] border border-[#2a4a00] rounded-lg p-3 text-xs text-[#aaff44]">
						✓ Exported:<br />
						<span className="text-text-mid break-all">{exportDone.split(/[\\/]/).pop()}</span>
						<button className="mt-1 text-y underline block"
							onClick={() => window.clipsta?.showInFolder(exportDone)}>Show in folder</button>
					</div>
				)}
				{exportError && (
					<div className="bg-red-900/30 border border-red-700 rounded-lg p-3 text-xs text-red-300">
						⚠ {exportError}
					</div>
				)}

				<button onClick={handleExport} disabled={exporting}
					className="btn-y justify-center w-full py-3 mt-auto disabled:opacity-50">
					{exporting
						? <><Loader2 size={16} className="animate-spin" /> Exporting…</>
						: <><Download size={16} /> Export Clip</>
					}
				</button>

				<p className="text-text-dim text-[10px] text-center">
					Export requires FFmpeg in PATH.<br />
					<a href="https://ffmpeg.org/download.html" className="text-y underline">Download FFmpeg</a>
				</p>
			</div>
		</div>
	);
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
	return (
		<div className="space-y-2">
			<p className="label">{title}</p>
			<div className="space-y-2">{children}</div>
		</div>
	);
}

function Select({ label, value, onChange, options }: {
	label: string; value: string; onChange: (v: string) => void; options: string[];
}) {
	return (
		<div className="space-y-1">
			<p className="text-text-dim text-[10px]">{label}</p>
			<select value={value} onChange={(e) => onChange(e.target.value)}
				className="input text-xs py-1.5 no-drag">
				{options.map((o) => <option key={o} value={o}>{o}</option>)}
			</select>
		</div>
	);
}

function Row({ label, val }: { label: string; val: string }) {
	return (
		<div className="flex justify-between text-xs">
			<span className="text-text-dim">{label}</span>
			<span className="text-white font-mono">{val}</span>
		</div>
	);
}

function AspectPreview({ ratio }: { ratio: string }) {
	const map: Record<string, string> = {
		"16:9": "w-20 h-[45px]", "9:16": "w-10 h-[71px]",
		"1:1": "w-14 h-14", "4:3": "w-16 h-12", "21:9": "w-24 h-[41px]",
	};
	const cls = map[ratio] ?? "w-16 h-9";
	return (
		<div className="flex flex-col items-center gap-1">
			<p className="label">Preview</p>
			<div className={`${cls} bg-muted border border-y rounded flex items-center justify-center`}>
				<Crop size={14} className="text-y" />
			</div>
			<p className="text-text-dim text-xs">{ratio}</p>
		</div>
	);
}

function formatTime(s: number) {
	if (!isFinite(s)) return "0:00";
	const m = Math.floor(s / 60);
	const sec = Math.floor(s % 60);
	const ms = Math.floor((s % 1) * 10);
	return `${m}:${String(sec).padStart(2, "0")}.${ms}`;
}
