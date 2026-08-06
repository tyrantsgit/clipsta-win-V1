import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
	Scissors, Download, RotateCcw, Volume2, VolumeX, FileUp,
	Play, Pause, SkipBack, SkipForward, Crop, Loader2, Trash2, FolderOpen, Upload, Pen, Check,
	Plus, ArrowUp, ArrowDown, GripHorizontal,
} from "lucide-react";
import type { AppSettings, ExportOpts, TimelineEntry, SpeedSegment, Transition, TransitionType } from "../../types";
import type { useCloudUpload } from "../../hooks/useCloudUpload";
import { toFileUrl, formatTime, sanitizeName, getTimeFromEvent } from "../../utils";
import { getCurrentWindow } from "@tauri-apps/api/window";
import bridge from "../../tauri-bridge";

interface Props {
	initialFile: string | null;
	settings: AppSettings;
	cloud: ReturnType<typeof useCloudUpload>;
	onExportDone?: (path: string) => void;
}

interface CutSeg { start: number; end: number }

// Undo/Redo snapshot of editor state
interface EditorSnapshot {
	trimIn: number;
	trimOut: number;
	cuts: CutSeg[];
	speedSegments: SpeedSegment[];
	transitions: Transition[];
	brightness: number;
	contrast: number;
	saturation: number;
}

const MAX_HISTORY = 50;

export default function EditorPage({ initialFile, settings, cloud, onExportDone }: Props) {
	const [timeline, setTimeline] = useState<TimelineEntry[]>(() => {
		if (!initialFile) return [];
		const name = initialFile.replace(/^.*[\\/]/, "");
		return [{ id: crypto.randomUUID(), path: initialFile, name, trimIn: 0, trimOut: 0 }];
	});
	const [activeIdx, setActiveIdx] = useState(0);
	const filePath = timeline[activeIdx]?.path ?? null;

	const [playing, setPlaying] = useState(false);
	const [currentTime, setCurrentTime] = useState(0);
	const [duration, setDuration] = useState(0);
	const [volume, setVolume] = useState(1);
	const [muted, setMuted] = useState(false);
	const [trimIn, setTrimIn] = useState(0);
	const [trimOut, setTrimOut] = useState(0);
	const [cuts, setCuts] = useState<CutSeg[]>([]);
	// Cut marking: first click sets cutMarkIn, second click completes the cut range
	const [cutMarkIn, setCutMarkIn] = useState<number | null>(null);
	const [exporting, setExporting] = useState(false);
	const [exportProgress, setExportProgress] = useState(0);
	const [exportDone, setExportDone] = useState<string | null>(null);
	const [exportError, setExportError] = useState<string | null>(null);
	const videoRef = useRef<HTMLVideoElement>(null);
	const [timelineScale, setTimelineScale] = useState(1);
	const [scissorPos, setScissorPos] = useState<number | null>(null);
	const [cutMode, setCutMode] = useState(false);
	const cutCount = cuts.length;
	const [speedMode, setSpeedMode] = useState(false);
	const [speedMarkIn, setSpeedMarkIn] = useState<number | null>(null);
	const [dragOver, setDragOver] = useState(false);
	const dragSrcRef = useRef<number | null>(null);
	const uploadJob = filePath ? cloud.queue.find((j) => j.path === filePath) : undefined;
	const uploadBusy = !!(uploadJob && (uploadJob.status === "queued" || uploadJob.status === "uploading"));
	const [showUploaded, setShowUploaded] = useState(false);

	useEffect(() => {
		if (uploadJob?.status === "done") {
			setShowUploaded(true);
			const t = setTimeout(() => setShowUploaded(false), 2000);
			return () => clearTimeout(t);
		}
	}, [uploadJob?.status]);

	const [renaming, setRenaming] = useState(false);
	const [renameValue, setRenameValue] = useState("");
	const renameInputRef = useRef<HTMLInputElement>(null);
	const [resetKey, setResetKey] = useState(0);
	const renameRef = useRef<HTMLDivElement>(null);

	const handleRename = async () => {
		const entry = timeline[activeIdx];
		if (!entry || !renameValue.trim()) return;
		const base = renameValue.trim();
		const ext = entry.path.replace(/^.*\./, "");
		const newName = ext ? `${base}.${ext}` : base;
		setTimeline((prev) => prev.map((e, i) => i === activeIdx ? { ...e, name: newName } : e));
		setRenaming(false);
	};

	const cancelRename = () => { setRenaming(false); };

	const [expFormat, setExpFormat] = useState("mp4");
	const [expResolution, setExpResolution] = useState(settings.resolution);
	const [expAspect, setExpAspect] = useState(settings.aspectRatio);

	// Video adjustments (CSS filter preview + FFmpeg filter on export)
	const [brightness, setBrightness] = useState(100);
	const [contrast, setContrast] = useState(100);
	const [saturation, setSaturation] = useState(100);

	// Speed Ramping
	const [speedSegments, setSpeedSegments] = useState<SpeedSegment[]>([]);

	// Transitions (at cut points)
	const [transitions, setTransitions] = useState<Transition[]>([]);

	// Undo/Redo system
	const [history, setHistory] = useState<EditorSnapshot[]>([]);
	const [historyIdx, setHistoryIdx] = useState(-1);
	const skipHistoryRef = useRef(false);

	const getSnapshot = useCallback((): EditorSnapshot => ({
		trimIn, trimOut, cuts, speedSegments, transitions, brightness, contrast, saturation,
	}), [trimIn, trimOut, cuts, speedSegments, transitions, brightness, contrast, saturation]);

	const pushHistory = useCallback(() => {
		if (skipHistoryRef.current) return;
		const snap = getSnapshot();
		setHistory((prev) => {
			const base = prev.slice(0, historyIdx + 1);
			const next = [...base, snap];
			return next.length > MAX_HISTORY ? next.slice(-MAX_HISTORY) : next;
		});
		setHistoryIdx((prev) => Math.min(prev + 1, MAX_HISTORY - 1));
	}, [getSnapshot, historyIdx]);

	const undo = useCallback(() => {
		if (historyIdx <= 0) return;
		const newIdx = historyIdx - 1;
		const snap = history[newIdx];
		if (!snap) return;
		skipHistoryRef.current = true;
		setTrimIn(snap.trimIn); setTrimOut(snap.trimOut);
		setCuts(snap.cuts); setSpeedSegments(snap.speedSegments);
		setTransitions(snap.transitions);
		setBrightness(snap.brightness); setContrast(snap.contrast); setSaturation(snap.saturation);
		setHistoryIdx(newIdx);
		skipHistoryRef.current = false;
	}, [history, historyIdx]);

	const redo = useCallback(() => {
		if (historyIdx >= history.length - 1) return;
		const newIdx = historyIdx + 1;
		const snap = history[newIdx];
		if (!snap) return;
		skipHistoryRef.current = true;
		setTrimIn(snap.trimIn); setTrimOut(snap.trimOut);
		setCuts(snap.cuts); setSpeedSegments(snap.speedSegments);
		setTransitions(snap.transitions);
		setBrightness(snap.brightness); setContrast(snap.contrast); setSaturation(snap.saturation);
		setHistoryIdx(newIdx);
		skipHistoryRef.current = false;
	}, [history, historyIdx]);

	// React to initialFile changes (when user clicks Edit from Library)
	useEffect(() => {
		if (!initialFile) return;
		const name = initialFile.replace(/^.*[\\/]/, "");
		setTimeline([{ id: crypto.randomUUID(), path: initialFile, name, trimIn: 0, trimOut: 0 }]);
		setActiveIdx(0);
		setTrimIn(0); setTrimOut(0); setCurrentTime(0); setDuration(0);
		setCuts([]); setExportDone(null); setExportError(null);
		setBrightness(100); setContrast(100); setSaturation(100);
		setSpeedSegments([]); setTransitions([]);
	}, [initialFile]);

	const selectClip = useCallback((idx: number) => {
		if (idx === activeIdx || idx < 0 || idx >= timeline.length) return;
		setTimeline((prev) => prev.map((e, i) => i === activeIdx ? { ...e, trimIn, trimOut } : e));
		setActiveIdx(idx);
	}, [activeIdx, trimIn, trimOut, timeline.length]);

	useEffect(() => {
		const entry = timeline[activeIdx];
		if (entry) {
			setTrimIn(entry.trimIn);
			if (entry.trimOut > 0) setTrimOut(entry.trimOut);
			setCurrentTime(0);
			setDuration(0);
			setCuts([]);
			setCutMode(false);
			setRenaming(false);
			setExportDone(null);
			setExportError(null);
			setPlaying(false);
		}
	}, [activeIdx, resetKey]); // eslint-disable-line react-hooks/exhaustive-deps

	useEffect(() => {
		const entry = timeline[activeIdx];
		if (entry && (entry.trimIn !== trimIn || entry.trimOut !== trimOut)) {
			setTimeline((prev) => prev.map((e, i) => i === activeIdx ? { ...e, trimIn, trimOut } : e));
		}
	}, [trimIn, trimOut, activeIdx, timeline]);

	const addToTimeline = useCallback(async (path: string) => {
		const name = path.replace(/^.*[\\/]/, "");
		const entry: TimelineEntry = { id: crypto.randomUUID(), path, name, trimIn: 0, trimOut: 0 };
		setTimeline((prev) => [...prev, entry]);
		setActiveIdx(timeline.length);
	}, [timeline.length]);

	// Tauri 2 native drag-drop: provides full file paths even in WebView2
	const addToTimelineRef = useRef(addToTimeline);
	addToTimelineRef.current = addToTimeline;
	useEffect(() => {
		const unlisten = getCurrentWindow().onDragDropEvent((event) => {
			if (event.payload.type === "drop") {
				const paths = (event.payload.paths ?? []).filter((p: string) =>
					/\.(webm|mp4|mkv|mov)$/i.test(p)
				);
				paths.forEach((p: string) => addToTimelineRef.current(p));
			}
		});
		return () => { unlisten.then((u) => u()); };
	}, []);

	// Export progress listener
	useEffect(() => {
		let unlistenFn: (() => void) | null = null;
		import("@tauri-apps/api/event").then(({ listen }) => {
			listen<number>("export:progress", (event) => {
				setExportProgress(event.payload);
			}).then((u) => { unlistenFn = u; });
		});
		return () => { unlistenFn?.(); };
	}, []);

	const removeFromTimeline = useCallback((idx: number) => {
		const newTimeline = timeline.filter((_, i) => i !== idx);
		setTimeline(newTimeline);
		if (newTimeline.length === 0) {
			setActiveIdx(-1); setCurrentTime(0); setDuration(0);
			setTrimIn(0); setTrimOut(0); setCuts([]); setCutMode(false); setPlaying(false);
		} else if (activeIdx >= newTimeline.length) {
			setActiveIdx(newTimeline.length - 1);
		} else if (activeIdx > idx) {
			setActiveIdx(activeIdx - 1);
		} else if (activeIdx === idx) {
			setActiveIdx(Math.min(idx, newTimeline.length - 1));
		}
	}, [timeline, activeIdx]);

	const moveClip = useCallback((idx: number, dir: -1 | 1) => {
		const to = idx + dir;
		if (to < 0 || to >= timeline.length) return;
		setTimeline((prev) => {
			const next = [...prev];
			[next[idx], next[to]] = [next[to], next[idx]];
			return next;
		});
		setActiveIdx(to);
	}, [timeline.length]);

	const onLoadedMetadata = () => {
		const v = videoRef.current;
		if (!v || !isFinite(v.duration)) return;
		setDuration((prev) => prev > 0 ? prev : v.duration);
		const entry = timeline[activeIdx];
		if (entry && entry.trimOut === 0) {
			setTrimOut(v.duration);
			setTimeline((prev) => prev.map((e, i) => i === activeIdx ? { ...e, trimOut: v.duration } : e));
		}
	};

	const onTimeUpdate = () => {
		const v = videoRef.current;
		if (!v) return;
		setCurrentTime(v.currentTime);
		if (trimOut > 0 && v.currentTime >= trimOut) {
			v.currentTime = trimIn;
			if (!playing) v.pause();
		}
		// Skip removed segments during playback
		if (playing && cuts.length > 0) {
			const inCut = cuts.find((c) => v.currentTime >= c.start && v.currentTime < c.end);
			if (inCut) {
				v.currentTime = inCut.end; // Jump to end of cut
			}
		}
		// Speed ramping: adjust playback rate based on current position
		const activeSeg = speedSegments.find((s: SpeedSegment) => v.currentTime >= s.start && v.currentTime <= s.end);
		const targetRate = activeSeg ? activeSeg.speed : 1;
		if (Math.abs(v.playbackRate - targetRate) > 0.01) {
			v.playbackRate = targetRate;
		}
	};

	const seek = (t: number) => {
		if (videoRef.current) {
			videoRef.current.currentTime = t;
			// Pause on manual seek so play button state stays consistent
			if (playing) {
				videoRef.current.pause();
				setPlaying(false);
			}
		}
		setCurrentTime(t);
	};

	const scrubRef = useRef(false);

	const handleTimelineMouseDown = (e: React.MouseEvent) => {
		const parent = e.currentTarget;
		const t = Math.max(0, Math.min(duration, getTimeFromEvent(e.clientX, parent, duration)));
		if (cutMode) {
			// Drag-to-cut: mousedown sets start, drag shows preview, mouseup sets end
			const startT = t;
			setCutMarkIn(startT); // Show the start marker immediately

			const parent = e.currentTarget;
			const moveHandler = (me: MouseEvent) => {
				const currentT = Math.max(0, Math.min(duration, getTimeFromEvent(me.clientX, parent, duration)));
				// Update cutMarkIn to show a live preview (the timeline will render the ghost region)
				setCutMarkIn(startT); // Keep start fixed, the rendering uses mouse position via a ref
				// Store current drag position for the ghost region
				(window as any).__clipsta_cut_drag_end = currentT;
				// Force re-render by updating a state that triggers the timeline to show the ghost
				setCurrentTime(videoRef.current?.currentTime ?? currentTime);
			};
			const upHandler = (me: MouseEvent) => {
				window.removeEventListener("mousemove", moveHandler);
				window.removeEventListener("mouseup", upHandler);
				const endT = Math.max(0, Math.min(duration, getTimeFromEvent(me.clientX, parent, duration)));
				const start = Math.min(startT, endT);
				const end = Math.max(startT, endT);
				if (end - start > 0.1) {
					setCuts((prev) => [...prev, { start, end }].sort((a, b) => a.start - b.start));
					pushHistory();
				}
				setCutMarkIn(null);
				(window as any).__clipsta_cut_drag_end = null;
			};
			window.addEventListener("mousemove", moveHandler);
			window.addEventListener("mouseup", upHandler);
			return;
		}
		if (speedMode) {
			if (speedMarkIn === null) {
				// First click: mark IN point
				setSpeedMarkIn(t);
			} else {
				// Second click: mark OUT point, create segment
				const start = Math.min(speedMarkIn, t);
				const end = Math.max(speedMarkIn, t);
				if (end - start > 0.1) {
					const seg: SpeedSegment = {
						id: crypto.randomUUID(),
						start,
						end,
						speed: 0.5, // Default to half speed (cinematic slow-mo)
					};
					setSpeedSegments((prev) => [...prev, seg].sort((a, b) => a.start - b.start));
					pushHistory();
				}
				setSpeedMarkIn(null);
				setSpeedMode(false);
			}
			return;
		}
		seek(t);
		scrubRef.current = true;
		const move = (me: MouseEvent) => {
			if (!scrubRef.current) return;
			const t2 = Math.max(0, Math.min(duration, getTimeFromEvent(me.clientX, parent, duration)));
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
		const t = Math.max(0, Math.min(duration, getTimeFromEvent(e.clientX, parent, duration)));
		// Right-click: if cut mode, set start marker; otherwise toggle cut mode + set start
		setCutMode(true);
		setCutMarkIn(t);
		setSpeedMode(false);
	};

	const togglePlay = () => {
		const v = videoRef.current;
		if (!v) return;
		if (playing) { v.pause(); setPlaying(false); }
		else {
			if (v.currentTime >= trimOut || v.currentTime < trimIn) v.currentTime = trimIn;
			v.play().catch(() => setExportError("Failed to play video file"));
			setPlaying(true);
		}
	};

	const setVol = (val: number) => {
		setVolume(val);
		if (videoRef.current) videoRef.current.volume = val;
	};

	const cutAtPlayhead = () => {
		const t = videoRef.current?.currentTime ?? currentTime;
		// Quick cut: place a 2-second cut centered on playhead
		const start = Math.max(trimIn, t - 1);
		const end = Math.min(trimOut || duration, t + 1);
		if (end - start > 0.1) {
			setCuts((prev) => [...prev, { start, end }].sort((a, b) => a.start - b.start));
			pushHistory();
		}
	};

	const removeCut = (idx: number) => { setCuts((prev) => prev.filter((_, i) => i !== idx)); };

	const pct = useCallback((t: number) => duration > 0 ? (t / duration) * 100 : 0, [duration]);

	// Keyboard shortcuts
	const togglePlayRef = useRef(togglePlay);
	togglePlayRef.current = togglePlay;
	const cutAtPlayheadRef = useRef(cutAtPlayhead);
	cutAtPlayheadRef.current = cutAtPlayhead;

	useEffect(() => {
		const onKey = (e: KeyboardEvent) => {
			if (e.target instanceof HTMLInputElement || e.target instanceof HTMLSelectElement || e.target instanceof HTMLTextAreaElement) return;
			const t = videoRef.current?.currentTime ?? 0;
			// Undo/Redo
			if ((e.ctrlKey || e.metaKey) && e.code === "KeyZ" && !e.shiftKey) { e.preventDefault(); undo(); return; }
			if ((e.ctrlKey || e.metaKey) && (e.code === "KeyY" || (e.code === "KeyZ" && e.shiftKey))) { e.preventDefault(); redo(); return; }
			switch (e.code) {
				case "Space": e.preventDefault(); togglePlayRef.current(); break;
				case "ArrowLeft": {
					e.preventDefault();
					const v = videoRef.current;
					if (v) {
						// Step back 1 frame (1/fps seconds)
						const fps = 60; // Default; could be detected from video metadata
						v.currentTime = Math.max(0, v.currentTime - 1 / fps);
						v.pause(); setPlaying(false);
						setCurrentTime(v.currentTime);
					}
					break;
				}
				case "ArrowRight": {
					e.preventDefault();
					const v = videoRef.current;
					if (v) {
						// Step forward 1 frame
						const fps = 60;
						v.currentTime = Math.min(duration, v.currentTime + 1 / fps);
						v.pause(); setPlaying(false);
						setCurrentTime(v.currentTime);
					}
					break;
				}
				case "KeyI": setTrimIn(t); if (videoRef.current) videoRef.current.currentTime = t; pushHistory(); break;
				case "KeyO": setTrimOut(t); pushHistory(); break;
				case "KeyX": e.preventDefault(); cutAtPlayheadRef.current(); pushHistory(); break;
				case "KeyS":
					e.preventDefault();
					if (!speedMode) {
						setSpeedMode(true); setCutMode(false); setSpeedMarkIn(t);
					} else if (speedMarkIn === null) {
						setSpeedMarkIn(t);
					} else {
						const start = Math.min(speedMarkIn, t);
						const end = Math.max(speedMarkIn, t);
						if (end - start > 0.1) {
							const seg: SpeedSegment = { id: crypto.randomUUID(), start, end, speed: 0.5 };
							setSpeedSegments((prev: SpeedSegment[]) => [...prev, seg].sort((a, b) => a.start - b.start));
							pushHistory();
						}
						setSpeedMarkIn(null);
						setSpeedMode(false);
					}
					break;
				case "Escape": setCutMode(false); setSpeedMode(false); setSpeedMarkIn(null); break;
			}
		};
		window.addEventListener("keydown", onKey);
		return () => window.removeEventListener("keydown", onKey);
	}, [undo, redo, pushHistory, speedMode, speedMarkIn, cutMarkIn]);

	// Drag-and-drop
	const dragCounterRef = useRef(0);

	const getDroppedPathsLocal = (dt: DataTransfer): string[] => {
		const paths: string[] = [];
		for (const f of Array.from(dt.files)) {
			const p = (f as any).path || f.name;
			if (p && /\.(webm|mp4|mkv|mov)$/i.test(p)) paths.push(p);
		}
		return paths;
	};

	const handleDrop = (e: React.DragEvent) => {
		e.preventDefault();
		dragCounterRef.current = 0;
		setDragOver(false);
		const paths = getDroppedPathsLocal(e.dataTransfer);
		paths.forEach((p) => addToTimeline(p));
	};

	const handleDragEnter = (e: React.DragEvent) => { e.preventDefault(); dragCounterRef.current++; setDragOver(true); };
	const handleDragOver = (e: React.DragEvent) => { e.preventDefault(); };
	const handleDragLeave = (e: React.DragEvent) => {
		e.preventDefault();
		dragCounterRef.current--;
		if (dragCounterRef.current <= 0) { dragCounterRef.current = 0; setDragOver(false); }
	};

	const handleBrowse = async () => {
		const path = await bridge.browseFile();
		if (path) addToTimeline(path);
	};

	const handleQuickMerge = async () => {
		if (timeline.length < 2) return;
		setPlaying(false);
		if (videoRef.current) videoRef.current.pause();
		const exportTimeline = timeline.map((e, i) => i === activeIdx ? { ...e, trimIn, trimOut } : e);
		const zeroDur = exportTimeline.find((e) => e.trimOut <= e.trimIn);
		if (zeroDur) { setExportError(`"${zeroDur.name}" has no duration loaded yet.`); return; }
		const folder = settings.outputFolder;
		if (!folder) { setExportError("No output folder set in Settings"); return; }
		const mergedName = exportTimeline.map((e) => sanitizeName(e.name.replace(/\.[^.]+$/, ""))).join(" + ");
		const outPath = `${folder}\\${mergedName}.mp4`;
		try { await bridge.ensureDir(folder); } catch { /* best effort */ }
		setExporting(true); setExportDone(null); setExportError(null); setExportProgress(0);
		try {
			const primaryPath = exportTimeline[0].path;
			const opts: ExportOpts = {
				format: "mp4", resolution: expResolution, aspectRatio: expAspect,
				fps: settings.fps, encoder: settings.encoder,
				timeline: exportTimeline.map((e) => ({ path: e.path, trimIn: e.trimIn, trimOut: e.trimOut })),
			};
			const out = await bridge.exportRecording(primaryPath, outPath, opts);
			if (out) {
				const name = out.replace(/^.*[\\/]/, "");
				setTimeline([{ id: crypto.randomUUID(), path: out, name, trimIn: 0, trimOut: 0 }]);
				setActiveIdx(0); setTrimIn(0); setTrimOut(0); setCurrentTime(0); setDuration(0);
				setCuts([]); setCutMode(false); setScissorPos(null);
				setExportDone(out); setExportError(null); setPlaying(false); setResetKey((k) => k + 1);
			} else { setExportError("Merge failed — no output file was created"); }
		} catch (e: any) { setExportError(e?.message ?? String(e)); }
		finally { setExporting(false); }
	};

	const handleExport = async () => {
		if (timeline.length === 0) return;
		setPlaying(false);
		if (videoRef.current) videoRef.current.pause();
		const exportTimeline = timeline.map((e, i) => i === activeIdx ? { ...e, trimIn, trimOut } : e);
		const baseName = exportTimeline.length === 1
			? (exportTimeline[0].name || exportTimeline[0].path.replace(/^.*[\\/]/, "")).replace(/\.[^.]+$/, "")
			: "combined";
		const ext = expFormat === "webm" ? "webm" : expFormat === "mkv" ? "mkv" : expFormat === "mov" ? "mov" : "mp4";
		const savePath = await bridge.browseSaveExport(`${baseName}_export.${ext}`);
		if (!savePath) return;
		setExporting(true); setExportDone(null); setExportError(null); setExportProgress(0);
		try {
			const primaryPath = exportTimeline[0].path;
			const exportingIdx = activeIdx;
			const opts: ExportOpts = {
				format: expFormat, resolution: expResolution, aspectRatio: expAspect,
				fps: settings.fps, encoder: settings.encoder,
				timeline: exportTimeline.length > 1 ? exportTimeline.map((e) => ({ path: e.path, trimIn: e.trimIn, trimOut: e.trimOut })) : undefined,
				trimStart: exportTimeline.length === 1 && trimIn > 0 ? trimIn : undefined,
				trimEnd: exportTimeline.length === 1 && trimOut < duration ? trimOut : undefined,
				cuts: exportTimeline.length === 1 && cuts.length > 0 ? cuts : undefined,
				brightness: brightness !== 100 ? brightness : undefined,
				contrast: contrast !== 100 ? contrast : undefined,
				saturation: saturation !== 100 ? saturation : undefined,
				speedSegments: speedSegments.length > 0 ? speedSegments.map((s) => ({ start: s.start, end: s.end, speed: s.speed })) : undefined,
				transitions: transitions.length > 0 ? transitions.map((t) => ({ time: t.time, type: t.type, duration: t.duration })) : undefined,
			};
			const out = await bridge.exportRecording(primaryPath, savePath, opts);
			setExportDone(out ?? null);
			if (out) {
				onExportDone?.(out);
				const name = out.replace(/^.*[\\/]/, "");
				if (exportTimeline.length > 1) {
					setTimeline([{ id: crypto.randomUUID(), path: out, name, trimIn: 0, trimOut: 0 }]);
					setActiveIdx(0);
				} else {
					setTimeline((prev) => prev.map((e, i) => i === exportingIdx ? { ...e, path: out, name, trimIn: 0, trimOut: 0 } : e));
				}
				setTrimIn(0); setTrimOut(0); setCurrentTime(0); setDuration(0);
				setCuts([]); setCutMode(false); setScissorPos(null); setExportError(null); setPlaying(false);
				setResetKey((k) => k + 1);
			} else { setExportError("Export failed — no output file was created"); }
		} catch (e: any) { setExportError(e?.message ?? String(e)); }
		finally { setExporting(false); }
	};

	const ticks = useMemo(() => {
		const tickInterval = Math.max(1, Math.floor(10 / timelineScale));
		const result: number[] = [];
		if (duration > 0 && isFinite(duration)) {
			const maxTicks = 10000;
			for (let t = 0, count = 0; t <= duration && count < maxTicks; t += tickInterval, count++) result.push(t);
		}
		return result;
	}, [duration, timelineScale]);

	// Empty state
	if (!filePath) {
		return (
			<div
				className={`flex-1 flex items-center justify-center flex-col gap-4 text-center p-8 transition-colors ${dragOver ? "bg-[#1c1c00]" : ""}`}
				onDrop={handleDrop} onDragEnter={handleDragEnter} onDragOver={handleDragOver} onDragLeave={handleDragLeave}
			>
				<div className={`rounded-2xl border-2 border-dashed p-12 flex flex-col items-center gap-4 transition-all duration-200 ${dragOver ? "border-y bg-[#2a2a00] scale-105 shadow-[0_0_40px_rgba(212,240,0,0.15)]" : "border-border hover:border-text-dim"}`}>
					<Scissors size={48} className={`transition-colors ${dragOver ? "text-y" : "text-text-dim opacity-30"}`} />
					<div>
						<p className={`text-xl font-bold transition-colors ${dragOver ? "text-y" : "text-white"}`}>
							{dragOver ? "Drop to start editing" : "Drop a video here"}
						</p>
						<p className="text-text-dim text-sm mt-1">or open from the Library to start editing</p>
					</div>
					<button onClick={handleBrowse} className="btn-y mt-2"><FolderOpen size={14} /> Browse Files</button>
				</div>
			</div>
		);
	}

	return (
		<EditorLayout
			timeline={timeline} activeIdx={activeIdx} filePath={filePath}
			selectClip={selectClip} addToTimeline={addToTimeline} removeFromTimeline={removeFromTimeline}
			moveClip={moveClip} handleBrowse={handleBrowse} handleDrop={handleDrop}
			handleDragEnter={handleDragEnter} handleDragOver={handleDragOver} handleDragLeave={handleDragLeave}
			dragOver={dragOver} dragSrcRef={dragSrcRef} setTimeline={setTimeline} setActiveIdx={setActiveIdx}
			videoRef={videoRef} onLoadedMetadata={onLoadedMetadata} onTimeUpdate={onTimeUpdate}
			playing={playing} setPlaying={setPlaying} exportError={exportError} setExportError={setExportError}
			muted={muted} setMuted={setMuted} volume={volume} setVol={setVol}
			currentTime={currentTime} duration={duration} trimIn={trimIn} trimOut={trimOut}
			setTrimIn={setTrimIn} setTrimOut={setTrimOut} seek={seek}
			cuts={cuts} setCuts={setCuts} cutMode={cutMode} setCutMode={setCutMode}
			cutCount={cutCount} scissorPos={scissorPos} ticks={ticks} pct={pct}
			speedMode={speedMode} setSpeedMode={setSpeedMode} speedMarkIn={speedMarkIn} setSpeedMarkIn={setSpeedMarkIn}
			cutMarkIn={cutMarkIn} setCutMarkIn={setCutMarkIn}
			handleTimelineMouseDown={handleTimelineMouseDown} handleTimelineContext={handleTimelineContext}
			togglePlay={togglePlay} timelineScale={timelineScale} setTimelineScale={setTimelineScale}
			cutAtPlayhead={cutAtPlayhead} removeCut={removeCut}
			renaming={renaming} setRenaming={setRenaming} renameValue={renameValue} setRenameValue={setRenameValue}
			renameInputRef={renameInputRef} renameRef={renameRef} handleRename={handleRename} cancelRename={cancelRename}
			handleQuickMerge={handleQuickMerge} handleExport={handleExport}
			exporting={exporting} exportDone={exportDone} exportProgress={exportProgress}
			expFormat={expFormat} setExpFormat={setExpFormat}
			expResolution={expResolution} setExpResolution={setExpResolution}
			expAspect={expAspect} setExpAspect={setExpAspect}
			brightness={brightness} contrast={contrast} saturation={saturation}
			setBrightness={setBrightness} setContrast={setContrast} setSaturation={setSaturation}
			cloud={cloud} uploadJob={uploadJob} uploadBusy={uploadBusy} showUploaded={showUploaded}
			settings={settings}
			speedSegments={speedSegments} setSpeedSegments={setSpeedSegments}
			transitions={transitions} setTransitions={setTransitions}
			history={history} historyIdx={historyIdx}
			undo={undo} redo={redo} pushHistory={pushHistory}
		/>
	);
}


// ── EditorLayout: main editing UI ───────────────────────────────────────────
function EditorLayout(props: any) {
	const {
		timeline, activeIdx, filePath,
		selectClip, removeFromTimeline, moveClip, handleBrowse,
		handleDrop, handleDragEnter, handleDragOver, handleDragLeave,
		dragOver, dragSrcRef, setTimeline, setActiveIdx,
		videoRef, onLoadedMetadata, onTimeUpdate,
		playing, setPlaying, exportError, setExportError,
		muted, setMuted, volume, setVol,
		currentTime, duration, trimIn, trimOut,
		setTrimIn, setTrimOut, seek,
		cuts, setCuts, cutMode, setCutMode,
		cutCount, scissorPos, ticks, pct,
		speedMode, setSpeedMode, speedMarkIn, setSpeedMarkIn,
		cutMarkIn, setCutMarkIn,
		handleTimelineMouseDown, handleTimelineContext,
		togglePlay, timelineScale, setTimelineScale,
		cutAtPlayhead, removeCut,
		renaming, setRenaming, renameValue, setRenameValue,
		renameInputRef, renameRef, handleRename, cancelRename,
		handleQuickMerge, handleExport,
		exporting, exportDone, exportProgress,
		expFormat, setExpFormat,
		expResolution, setExpResolution,
		expAspect, setExpAspect,
		brightness, contrast, saturation,
		setBrightness, setContrast, setSaturation,
		cloud, uploadJob, uploadBusy, showUploaded,
		settings,
		speedSegments, setSpeedSegments,
		transitions, setTransitions,
		history, historyIdx,
		undo, redo, pushHistory,
	} = props;

	return (
		<div
			className={`flex-1 flex overflow-hidden transition-colors ${dragOver ? "bg-[#1c1c00]" : ""}`}
			onDrop={handleDrop} onDragEnter={handleDragEnter} onDragOver={handleDragOver} onDragLeave={handleDragLeave}
		>
			{dragOver && (
				<div className="absolute inset-0 z-50 flex items-center justify-center bg-black/70 pointer-events-none">
					<div className="rounded-2xl border-2 border-dashed border-y p-10 text-center bg-[#1c1c00]/80 backdrop-blur-sm">
						<FileUp size={40} className="mx-auto mb-3 text-y" />
						<p className="text-y text-lg font-bold">{timeline.length > 0 ? "Drop to add to timeline" : "Drop video to start"}</p>
						<p className="text-text-dim text-sm mt-1">Supports MP4, WebM, MKV, MOV</p>
					</div>
				</div>
			)}

			{/* Main editor area */}
			<div className="flex-1 flex flex-col overflow-hidden">
				{/* Clip strip */}
				{timeline.length > 0 && (
					<div className="flex-shrink-0 bg-[#0d0d0d] border-b border-border px-3 py-2">
						<div className="flex gap-2 items-center overflow-x-auto">
							{timeline.map((clip: any, idx: number) => (
								<div
									key={clip.id}
									onClick={() => selectClip(idx)}
									draggable={timeline.length > 1}
									onDragStart={(e: React.DragEvent) => {
										e.dataTransfer.effectAllowed = "move";
										e.dataTransfer.setData("text/plain", String(idx));
										(e.currentTarget as HTMLElement).style.opacity = "0.4";
									}}
									onDragEnd={(e: React.DragEvent) => { (e.currentTarget as HTMLElement).style.opacity = "1"; }}
									onDragOver={(e: React.DragEvent) => { e.preventDefault(); e.dataTransfer.dropEffect = "move"; }}
									onDrop={(e: React.DragEvent) => {
										e.preventDefault(); e.stopPropagation();
										const fromIdx = parseInt(e.dataTransfer.getData("text/plain"));
										if (isNaN(fromIdx) || fromIdx === idx) return;
										setTimeline((prev: any[]) => {
											const next = [...prev]; const [moved] = next.splice(fromIdx, 1); next.splice(idx, 0, moved); return next;
										});
									}}
									className={`flex-shrink-0 flex items-center gap-2 px-3 py-1.5 rounded text-xs transition-colors border select-none cursor-grab active:cursor-grabbing ${
										idx === activeIdx ? "bg-y/10 border-y/50 text-y" : "bg-muted border-border text-text-mid hover:border-y/30 hover:text-white"
									}`}
								>
									<span className="font-mono truncate max-w-[120px]">{clip.name}</span>
									<span className="text-[10px] opacity-60 tabular-nums">{clip.trimOut > 0 ? formatTime(clip.trimOut - clip.trimIn) : "—"}</span>
								</div>
							))}
						</div>
						<div onClick={handleBrowse} className={`mt-2 flex items-center justify-center gap-2 py-3 px-4 rounded-lg border-2 border-dashed cursor-pointer transition-colors ${
							dragOver ? "border-y bg-y/10 text-y" : "border-border text-text-dim hover:border-y/50 hover:text-y/80"
						}`}>
							<Plus size={18} />
							<p className="text-xs font-semibold text-center">{dragOver ? "Drop to add to timeline" : "Drag video here or click to browse"}</p>
						</div>
					</div>
				)}

				{/* Video preview */}
				<div className="flex-1 bg-black flex items-center justify-center overflow-hidden min-h-0 p-4 relative">
					<div className="relative bg-black flex items-center justify-center overflow-hidden rounded-lg border border-border"
						style={{
							aspectRatio: expAspect === "9:16" ? "9/16" : expAspect === "1:1" ? "1/1" : expAspect === "4:5" ? "4/5" : "16/9",
							maxWidth: "100%",
							maxHeight: "100%",
						}}>
						{filePath && (
							<video
								key={filePath}
								ref={videoRef}
								src={toFileUrl(filePath)}
								className="w-full h-full"
								style={{ objectFit: "cover", filter: `brightness(${brightness}%) contrast(${contrast}%) saturate(${saturation}%)` }}
								onLoadedMetadata={onLoadedMetadata}
								onTimeUpdate={onTimeUpdate}
								onEnded={() => setPlaying(false)}
								onError={() => setExportError(`Cannot load file: ${filePath}`)}
							/>
						)}
						{/* Speed indicator overlay */}
						{speedSegments.some((s: SpeedSegment) => currentTime >= s.start && currentTime <= s.end && s.speed !== 1) && (
							<div className="absolute top-3 left-3 bg-black/70 text-y text-[11px] font-bold px-2 py-1 rounded z-10">
								{speedSegments.find((s: SpeedSegment) => currentTime >= s.start && currentTime <= s.end)?.speed ?? 1}x
							</div>
						)}
						<VolumeSync videoRef={videoRef} muted={muted} volume={volume} />
					</div>
				</div>

				{/* Timeline controls */}
				<TimelineControls
					cutMode={cutMode} setCutMode={setCutMode} cutCount={cutCount} setCuts={setCuts}
					speedMode={speedMode} setSpeedMode={setSpeedMode} speedMarkIn={speedMarkIn} setSpeedMarkIn={setSpeedMarkIn}
					speedSegments={speedSegments} setSpeedSegments={setSpeedSegments}
					transitions={transitions}
					cutMarkIn={cutMarkIn} setCutMarkIn={setCutMarkIn}
					duration={duration} ticks={ticks} pct={pct} trimIn={trimIn} trimOut={trimOut}
					cuts={cuts} scissorPos={scissorPos} currentTime={currentTime}
					handleTimelineMouseDown={handleTimelineMouseDown} handleTimelineContext={handleTimelineContext}
					setTrimIn={setTrimIn} setTrimOut={setTrimOut} seek={seek}
					togglePlay={togglePlay} playing={playing} muted={muted} setMuted={setMuted}
					volume={volume} setVol={setVol} timelineScale={timelineScale} setTimelineScale={setTimelineScale}
					videoRef={videoRef} timeline={timeline} activeIdx={activeIdx} selectClip={selectClip}
					pushHistory={pushHistory}
				/>
			</div>

			{/* Right panel */}
			<RightPanel
				timeline={timeline} activeIdx={activeIdx} filePath={filePath}
				selectClip={selectClip} removeFromTimeline={removeFromTimeline} moveClip={moveClip}
				dragSrcRef={dragSrcRef} setTimeline={setTimeline} setActiveIdx={setActiveIdx}
				trimIn={trimIn} trimOut={trimOut} duration={duration}
				cuts={cuts} setCuts={setCuts} removeCut={removeCut}
				renaming={renaming} setRenaming={setRenaming} renameValue={renameValue} setRenameValue={setRenameValue}
				renameInputRef={renameInputRef} renameRef={renameRef} handleRename={handleRename} cancelRename={cancelRename}
				handleQuickMerge={handleQuickMerge} handleExport={handleExport}
				exporting={exporting} exportDone={exportDone} exportError={exportError} setExportError={setExportError} exportProgress={exportProgress}
				expFormat={expFormat} setExpFormat={setExpFormat}
				expResolution={expResolution} setExpResolution={setExpResolution}
				expAspect={expAspect} setExpAspect={setExpAspect}
				brightness={brightness} setBrightness={setBrightness}
				contrast={contrast} setContrast={setContrast}
				saturation={saturation} setSaturation={setSaturation}
				cloud={cloud} uploadJob={uploadJob} uploadBusy={uploadBusy} showUploaded={showUploaded}
				settings={settings}
				speedSegments={speedSegments} setSpeedSegments={setSpeedSegments}
				transitions={transitions} setTransitions={setTransitions}
				currentTime={currentTime} undo={undo} redo={redo} pushHistory={pushHistory}
				history={history} historyIdx={historyIdx}
			/>
		</div>
	);
}


// ── Timeline Controls ───────────────────────────────────────────────────────
function TimelineControls(props: any) {
	const {
		cutMode, setCutMode, cutCount, setCuts,
		speedMode, setSpeedMode, speedMarkIn, setSpeedMarkIn,
		speedSegments, setSpeedSegments,
		transitions,
		cutMarkIn, setCutMarkIn,
		duration, ticks, pct, trimIn, trimOut,
		cuts, scissorPos, currentTime,
		handleTimelineMouseDown, handleTimelineContext,
		setTrimIn, setTrimOut, seek,
		togglePlay, playing, muted, setMuted,
		volume, setVol, timelineScale, setTimelineScale,
		videoRef, timeline, activeIdx, selectClip,
		pushHistory,
	} = props;

	return (
		<div className="bg-[#0d0d0d] border-t border-border px-4 py-3 space-y-2 flex-shrink-0">
			{/* Mode toolbar */}
			<div className="flex items-center gap-2">
				<button
					onClick={() => { setCutMode((m: boolean) => !m); setSpeedMode(false); setSpeedMarkIn(null); setCutMarkIn(null); }}
					className={`flex items-center gap-1.5 px-2.5 py-1 rounded text-xs font-semibold transition-all ${
						cutMode ? "bg-red-500/20 text-red-400 border border-red-500/50" : "text-text-mid border border-border hover:border-red-400/40 hover:text-red-400/80"
					}`}
					title="Cut mode — click timeline for start, click again for end [X]"
				>
					<Scissors size={13} /><span>Cut</span>
				</button>
				<button
					onClick={() => { setSpeedMode((m: boolean) => !m); setCutMode(false); if (speedMode) setSpeedMarkIn(null); }}
					className={`flex items-center gap-1.5 px-2.5 py-1 rounded text-xs font-semibold transition-all ${
						speedMode ? "bg-blue-500/20 text-blue-400 border border-blue-500/50" : "text-text-mid border border-border hover:border-blue-400/40 hover:text-blue-400/80"
					}`}
					title="Speed mode — click timeline to mark IN, click again for OUT [S]"
				>
					<span className="text-[11px]">⚡</span><span>Speed</span>
				</button>
				{cutCount > 0 && (
					<span className="text-[11px] text-y font-mono flex items-center gap-1 bg-y/10 px-1.5 py-0.5 rounded">
						{cutCount} cut{cutCount !== 1 ? "s" : ""}
					</span>
				)}
				{speedSegments.length > 0 && (
					<span className="text-[11px] text-blue-400 font-mono flex items-center gap-1 bg-blue-500/10 px-1.5 py-0.5 rounded">
						{speedSegments.length} speed
					</span>
				)}
				{/* Speed mode guidance */}
				{speedMode && (
					<span className="text-[11px] text-blue-300 animate-pulse ml-2">
						{speedMarkIn === null ? "⬇ Click timeline to set START" : `⬇ Click timeline to set END (start: ${formatTime(speedMarkIn)})`}
					</span>
				)}
				{/* Cut mode guidance */}
				{cutMode && (
					<span className="text-[11px] text-red-300 ml-2">
						✂ Drag on timeline to mark cut area
					</span>
				)}
			</div>

			{/* Time ruler */}
			{duration > 0 && (
				<div className="relative h-4 overflow-hidden select-none" style={{ fontSize: 0 }}>
					{ticks.map((t: number) => (
						<div key={t} className="absolute top-0 flex flex-col items-start" style={{ left: `${pct(t)}%`, transform: "translateX(-50%)" }}>
							<div className="h-2 w-px bg-muted" />
							<span className="text-[10px] text-text-dim font-mono mt-0.5" style={{ fontSize: 9 }}>
								{Math.floor(t / 60)}:{String(Math.floor(t % 60)).padStart(2, "0")}
							</span>
						</div>
					))}
				</div>
			)}

			{/* Combined clip track when 2+ clips */}
			{timeline.length > 1 && (
				<div className="flex h-7 rounded overflow-hidden gap-px">
					{(() => {
						const totalDur = timeline.reduce((s: number, e: any) => s + (e.trimOut - e.trimIn), 0);
						const clipColors = ["#D4F00033", "#00B4D833", "#FF6B6B33", "#51CF6633", "#FFA94D33", "#CC99FF33"];
						return timeline.map((clip: any, idx: number) => {
							const clipDur = clip.trimOut - clip.trimIn;
							const p = totalDur > 0 ? (clipDur / totalDur) * 100 : 0;
							const isActive = idx === activeIdx;
							return (
								<div key={clip.id} onClick={() => selectClip(idx)}
									className={`relative flex items-center px-2 text-[10px] font-mono truncate cursor-pointer transition-all ${isActive ? "font-bold ring-1 ring-yellow-400/70 z-10" : "hover:brightness-125"}`}
									style={{ width: `${p}%`, backgroundColor: isActive ? "#D4F00022" : clipColors[idx % clipColors.length] }}
									title={`${clip.name} (${formatTime(clipDur)})`}
								>
									<span className="truncate">{clip.name}</span>
									<span className="ml-auto text-[8px] opacity-70 flex-shrink-0">{formatTime(clipDur)}</span>
								</div>
							);
						});
					})()}
				</div>
			)}

			{/* Thumbnail strip — video frame preview */}
			{duration > 0 && <ThumbnailStrip videoRef={videoRef} duration={duration} filePath={timeline[activeIdx]?.path ?? null} />}

			{/* Timeline legend (shows on hover) */}
			<div className="relative group/legend">
				<div className="flex items-center gap-3 text-[10px] text-text-dim opacity-0 group-hover/legend:opacity-100 transition-opacity duration-300 absolute -top-5 left-0 z-50 bg-[#0d0d0d] px-2 py-1 rounded border border-border shadow-xl">
					<span className="flex items-center gap-1"><span className="w-2 h-2 bg-[#D4F000] rounded-sm" /> Trim</span>
					<span className="flex items-center gap-1"><span className="w-2 h-2 bg-blue-500 rounded-sm" /> Speed</span>
					<span className="flex items-center gap-1"><span className="w-2 h-2 bg-red-500 rounded-sm" /> Cut</span>
					<span className="flex items-center gap-1"><span className="w-2 h-2 bg-purple-500 rounded-sm rotate-45" /> Transition</span>
					<span className="flex items-center gap-1"><span className="w-2 h-2 bg-white rounded-full" /> Playhead</span>
				</div>

				{/* Main timeline scrubber — 80px tall for comfortable editing */}
				<div
					className={`relative h-16 bg-muted rounded-lg overflow-hidden select-none ${cutMode ? "cursor-crosshair" : speedMode ? "cursor-cell" : "cursor-pointer"} group`}
					onMouseDown={handleTimelineMouseDown} onContextMenu={handleTimelineContext}
				>
					{ticks.map((t: number) => (<div key={t} className="absolute top-0 h-full w-px bg-black/20" style={{ left: `${pct(t)}%` }} />))}
					{/* Trim region */}
					<div className="absolute top-0 h-full bg-[#D4F00015]" style={{ left: `${pct(trimIn)}%`, width: `${pct(trimOut) - pct(trimIn)}%` }}>
						<div className="absolute left-0 top-2 -translate-x-1/2 bg-y text-black text-[8px] font-bold px-1.5 py-0.5 rounded shadow-lg pointer-events-none z-30">IN</div>
						<div className="absolute right-0 top-2 translate-x-1/2 bg-y text-black text-[8px] font-bold px-1.5 py-0.5 rounded shadow-lg pointer-events-none z-30">OUT</div>
					</div>

					{/* Speed segments — SVG curve visualization */}
					{speedSegments.length > 0 && (
						<svg className="absolute bottom-0 left-0 w-full h-6 z-[4] pointer-events-none" preserveAspectRatio="none">
							{speedSegments.map((seg: any) => {
								const x1 = pct(seg.start);
								const x2 = pct(seg.end);
								const w = x2 - x1;
								// Curve height based on speed deviation from 1x (inverted: slower = taller)
								const intensity = seg.speed < 1 ? (1 - seg.speed) * 100 : (seg.speed - 1) * 50;
								const h = Math.min(100, Math.max(20, intensity));
								const color = seg.speed < 1 ? "#3b82f6" : "#f59e0b"; // blue for slow, amber for fast
								// SVG path: ease-in curve at start, flat middle, ease-out at end
								const easeW = Math.min(w * 0.25, 3); // 25% of width for easing, max 3%
								return (
									<g key={seg.id}>
										<path
											d={`M ${x1},100 C ${x1 + easeW},100 ${x1 + easeW},${100 - h} ${x1 + easeW * 2},${100 - h} L ${x2 - easeW * 2},${100 - h} C ${x2 - easeW},${100 - h} ${x2 - easeW},100 ${x2},100 Z`}
											fill={`${color}33`}
											stroke={color}
											strokeWidth="1.5"
											vectorEffect="non-scaling-stroke"
										/>
										<text
											x={(x1 + x2) / 2}
											y={100 - h / 2}
											textAnchor="middle"
											dominantBaseline="middle"
											fill={color}
											fontSize="9"
											fontWeight="bold"
											className="pointer-events-none"
										>
											{seg.speed}x
										</text>
									</g>
								);
							})}
						</svg>
					)}

					{/* Speed segment draggable edges */}
					{speedSegments.map((seg: any) => (
						<SpeedEdgeHandle
							key={`${seg.id}-left`}
							position={pct(seg.start)}
							side="left"
							onDrag={(t: number) => {
								const clamped = Math.max(0, Math.min(seg.end - 0.2, t));
								setSpeedSegments((prev: any[]) => prev.map((s: any) => s.id === seg.id ? { ...s, start: clamped } : s));
							}}
							onDragEnd={pushHistory}
							duration={duration}
						/>
					))}
					{speedSegments.map((seg: any) => (
						<SpeedEdgeHandle
							key={`${seg.id}-right`}
							position={pct(seg.end)}
							side="right"
							onDrag={(t: number) => {
								const clamped = Math.min(duration, Math.max(seg.start + 0.2, t));
								setSpeedSegments((prev: any[]) => prev.map((s: any) => s.id === seg.id ? { ...s, end: clamped } : s));
							}}
							onDragEnd={pushHistory}
							duration={duration}
						/>
					))}

					{/* Speed mark-in indicator (pulsing vertical line) */}
					{speedMarkIn !== null && (
						<div className="absolute top-0 h-full w-0.5 bg-blue-400 z-[15] animate-pulse" style={{ left: `${pct(speedMarkIn)}%` }}>
							<div className="absolute top-1 -translate-x-1/2 bg-blue-400 text-black text-[8px] font-bold px-1 rounded">START</div>
						</div>
					)}

					{/* Transition markers at cut points — diamonds with type label */}
					{transitions.map((tr: any, i: number) => (
						<div key={tr.id || i} className="absolute top-1 z-[7] flex flex-col items-center pointer-events-none -translate-x-1/2"
							style={{ left: `${pct(tr.time)}%` }}
						>
							<div className="w-3.5 h-3.5 rotate-45 bg-purple-500/70 border border-purple-300/80 shadow-lg" />
							<span className="text-[7px] text-purple-300 font-medium mt-0.5 -rotate-0">{tr.transition_type || tr.type}</span>
						</div>
					))}

					{/* Cut areas — clearly shows what will be removed */}
					{cuts.map((c: any, i: number) => (
						<div key={i} className="absolute top-0 h-full z-[5] group/cut"
							style={{ left: `${pct(c.start)}%`, width: `${Math.max(0.5, pct(c.end) - pct(c.start))}%` }}
						>
							{/* Red striped background showing removed area */}
							<div className="absolute inset-0 bg-red-900/50 rounded-sm" style={{
								backgroundImage: "repeating-linear-gradient(135deg, transparent, transparent 4px, rgba(239,68,68,0.25) 4px, rgba(239,68,68,0.25) 6px)",
							}} />
							{/* Left edge (draggable) */}
							<div
								className="absolute left-0 top-0 h-full w-2 -translate-x-1/2 cursor-ew-resize z-10 group/edge"
								onMouseDown={(e) => {
									e.stopPropagation();
									const parent = e.currentTarget.parentElement!.parentElement!;
									const move = (me: MouseEvent) => {
										const rect = parent.getBoundingClientRect();
										const newT = Math.max(0, Math.min(c.end - 0.2, ((me.clientX - rect.left) / rect.width) * duration));
										setCuts((prev: CutSeg[]) => prev.map((cut, idx) => idx === i ? { ...cut, start: newT } : cut));
									};
									const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); pushHistory(); };
									window.addEventListener("mousemove", move);
									window.addEventListener("mouseup", up);
								}}
							>
								<div className="w-1 h-full bg-red-400 group-hover/edge:bg-red-300 group-hover/edge:shadow-[0_0_8px_#ef4444] transition-all mx-auto rounded-full" />
							</div>
							{/* Right edge (draggable) */}
							<div
								className="absolute right-0 top-0 h-full w-2 translate-x-1/2 cursor-ew-resize z-10 group/edge"
								onMouseDown={(e) => {
									e.stopPropagation();
									const parent = e.currentTarget.parentElement!.parentElement!;
									const move = (me: MouseEvent) => {
										const rect = parent.getBoundingClientRect();
										const newT = Math.min(duration, Math.max(c.start + 0.2, ((me.clientX - rect.left) / rect.width) * duration));
										setCuts((prev: CutSeg[]) => prev.map((cut, idx) => idx === i ? { ...cut, end: newT } : cut));
									};
									const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); pushHistory(); };
									window.addEventListener("mousemove", move);
									window.addEventListener("mouseup", up);
								}}
							>
								<div className="w-1 h-full bg-red-400 group-hover/edge:bg-red-300 group-hover/edge:shadow-[0_0_8px_#ef4444] transition-all mx-auto rounded-full" />
							</div>
							{/* Center label + delete */}
							<div className="absolute inset-0 flex items-center justify-center pointer-events-none">
								<span className="text-red-200 text-[11px] font-bold bg-red-900/80 px-2 py-0.5 rounded flex items-center gap-1">
									✂ {formatTime(c.end - c.start)}
								</span>
							</div>
							{/* Delete button on hover */}
							<button
								onClick={(e) => { e.stopPropagation(); setCuts((prev: CutSeg[]) => prev.filter((_, idx) => idx !== i)); pushHistory(); }}
								className="absolute -top-2 left-1/2 -translate-x-1/2 bg-red-600 text-white rounded-full w-4 h-4 text-[8px] flex items-center justify-center opacity-0 group-hover/cut:opacity-100 transition-opacity z-30 cursor-pointer hover:bg-red-500 shadow"
								title="Remove cut"
							>✕</button>
						</div>
					))}
					{/* Live drag ghost while creating a new cut */}
					{cutMarkIn !== null && (() => {
						const dragEnd = (window as any).__clipsta_cut_drag_end;
						if (dragEnd == null) {
							// Just the start marker (no drag yet)
							return (
								<div className="absolute top-0 h-full w-1 bg-red-400 z-[15] animate-pulse" style={{ left: `${pct(cutMarkIn)}%` }}>
									<div className="absolute top-1 -translate-x-1/2 bg-red-500 text-white text-[8px] font-bold px-1.5 py-0.5 rounded shadow">✂ DRAG →</div>
								</div>
							);
						}
						// Show ghost region between start and current drag position
						const start = Math.min(cutMarkIn, dragEnd);
						const end = Math.max(cutMarkIn, dragEnd);
						return (
							<div className="absolute top-0 h-full z-[14] pointer-events-none"
								style={{ left: `${pct(start)}%`, width: `${pct(end) - pct(start)}%` }}
							>
								<div className="absolute inset-0 bg-red-500/30 border-2 border-dashed border-red-400 rounded" />
								<div className="absolute inset-0 flex items-center justify-center">
									<span className="text-red-200 text-[10px] font-bold bg-red-900/70 px-2 py-0.5 rounded">
										✂ {formatTime(end - start)}
									</span>
								</div>
							</div>
						);
					})()}
					{scissorPos !== null && (
						<div className="absolute top-1/2 -translate-y-1/2 -translate-x-1/2 z-20 pointer-events-none" style={{ left: `${pct(scissorPos)}%` }}>
							<Scissors size={16} className="text-y" />
						</div>
					)}
					{/* Playhead — draggable */}
					<div
						className="absolute top-0 w-4 h-full z-10 -translate-x-1/2 cursor-grab active:cursor-grabbing"
						style={{ left: `${pct(currentTime)}%` }}
						onMouseDown={(e) => {
							e.stopPropagation();
							const parent = e.currentTarget.parentElement!;
							const move = (me: MouseEvent) => {
								const rect = parent.getBoundingClientRect();
								const t = Math.max(0, Math.min(duration, ((me.clientX - rect.left) / rect.width) * duration));
								seek(t);
							};
							const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); };
							window.addEventListener("mousemove", move);
							window.addEventListener("mouseup", up);
						}}
					>
						<div className="w-0.5 h-full bg-white shadow-[0_0_6px_rgba(255,255,255,0.5)] mx-auto">
							<div className="w-3.5 h-3.5 bg-white rounded-full -ml-[6px] -mt-[1px] shadow-lg border border-white/50" />
						</div>
					</div>
					{/* Trim handles */}
					<TrimHandle position={pct(trimIn)} onDrag={(t: number) => setTrimIn(Math.max(0, Math.min(trimOut - 0.5, t)))} duration={duration} />
					<TrimHandle position={pct(trimOut)} onDrag={(t: number) => setTrimOut(Math.min(duration, Math.max(trimIn + 0.5, t)))} duration={duration} />
				</div>
			</div>

			{/* Time display */}
			<div className="flex items-center justify-between gap-4">
				<div className="flex items-center gap-3 text-xs text-text-dim font-mono">
					<span className="flex items-center gap-1"><span className="text-y font-bold text-[11px]">IN</span>{formatTime(trimIn)}</span>
					<span className="text-white font-bold">{formatTime(currentTime)}</span>
					<span className="flex items-center gap-1"><span className="text-y font-bold text-[11px]">OUT</span>{formatTime(trimOut)}</span>
					<span className="text-text-mid text-[11px]">({formatTime(trimOut - trimIn)})</span>
				</div>
				<div className="flex items-center gap-2">
					<span className="text-[11px] text-text-dim">Zoom</span>
					<input type="range" min={1} max={5} step={0.5} value={timelineScale} onChange={(e) => setTimelineScale(Number(e.target.value))} className="w-16 accent-[#D4F000] no-drag" />
				</div>
			</div>

			{/* Playback controls — IN/OUT centered prominently */}
			<div className="flex items-center justify-center gap-3">
				{/* Set IN */}
				<button onClick={() => { const t = videoRef.current?.currentTime ?? 0; setTrimIn(t); if (videoRef.current) videoRef.current.currentTime = t; pushHistory(); }}
					className="flex items-center gap-1 px-3 py-1.5 rounded-lg border-2 border-y/40 bg-y/5 text-y text-xs font-bold hover:bg-y/15 hover:border-y transition-all" title="Set trim IN point [I]">
					<SkipBack size={12} /> IN
				</button>

				{/* Transport */}
				<button onClick={() => seek(trimIn)} className="text-text-mid hover:text-white transition-colors p-1" title="Jump to IN"><SkipBack size={14} /></button>
				<button onClick={togglePlay} className="w-10 h-10 rounded-full bg-y hover:bg-yd flex items-center justify-center transition-colors shadow-lg">
					{playing ? <Pause size={16} fill="black" className="text-black" /> : <Play size={16} fill="black" className="text-black ml-0.5" />}
				</button>
				<button onClick={() => seek(trimOut)} className="text-text-mid hover:text-white transition-colors p-1" title="Jump to OUT"><SkipForward size={14} /></button>

				{/* Set OUT */}
				<button onClick={() => { const t = videoRef.current?.currentTime ?? 0; setTrimOut(t); pushHistory(); }}
					className="flex items-center gap-1 px-3 py-1.5 rounded-lg border-2 border-y/40 bg-y/5 text-y text-xs font-bold hover:bg-y/15 hover:border-y transition-all" title="Set trim OUT point [O]">
					OUT <SkipForward size={12} />
				</button>
			</div>

			{/* Secondary controls */}
			<div className="flex items-center justify-between">
				<div className="flex items-center gap-2">
					<button onClick={() => setMuted((m: boolean) => !m)} className="text-text-mid hover:text-white transition-colors p-1">
						{muted ? <VolumeX size={14} /> : <Volume2 size={14} />}
					</button>
					<input type="range" min={0} max={1} step={0.05} value={muted ? 0 : volume} onChange={(e) => setVol(Number(e.target.value))} className="w-16 accent-[#D4F000] no-drag" />
					<div className="w-px h-5 bg-border mx-2" />
					<button onClick={() => { setTrimIn(0); setTrimOut(duration); }} className="text-text-dim hover:text-white transition-colors p-1 flex items-center gap-1 text-[11px]" title="Reset trim"><RotateCcw size={12} /> Reset</button>
				</div>
				<div className="text-[10px] text-text-dim hidden md:block">
					<span className="inline-flex items-center gap-2">
						<span><kbd className="bg-muted px-1 rounded">I</kbd> IN</span>
						<span><kbd className="bg-muted px-1 rounded">O</kbd> OUT</span>
						<span><kbd className="bg-muted px-1 rounded">X</kbd> Cut</span>
						<span><kbd className="bg-muted px-1 rounded">S</kbd> Speed</span>
						<span><kbd className="bg-muted px-1 rounded">←→</kbd> Frame</span>
						<span><kbd className="bg-muted px-1 rounded">Space</kbd> Play</span>
						<span><kbd className="bg-muted px-1 rounded">Ctrl+Z</kbd> Undo</span>
					</span>
				</div>
			</div>
		</div>
	);
}


// ── Right Panel ─────────────────────────────────────────────────────────────
function RightPanel(props: any) {
	const {
		timeline, activeIdx, filePath,
		selectClip, removeFromTimeline, moveClip,
		dragSrcRef, setTimeline, setActiveIdx,
		trimIn, trimOut, duration,
		cuts, setCuts, removeCut,
		renaming, setRenaming, renameValue, setRenameValue,
		renameInputRef, renameRef, handleRename, cancelRename,
		handleQuickMerge, handleExport,
		exporting, exportDone, exportError, setExportError, exportProgress,
		expFormat, setExpFormat,
		expResolution, setExpResolution,
		expAspect, setExpAspect,
		brightness, setBrightness,
		contrast, setContrast,
		saturation, setSaturation,

		cloud, uploadJob, uploadBusy, showUploaded,
		settings,
		speedSegments, setSpeedSegments,
		transitions, setTransitions,
		currentTime, undo, redo, pushHistory,
		history, historyIdx,
	} = props;

	return (
		<div className="w-72 flex-shrink-0 border-l border-border bg-[#0d0d0d] flex flex-col overflow-hidden">
			<div className="flex-1 overflow-y-auto p-4 space-y-4">
			{/* Timeline clips list */}
			<div className="space-y-2">
				<span className="text-[11px] text-text-dim">{timeline.length} clip{timeline.length !== 1 ? "s" : ""}</span>
				{timeline.length === 0 ? (
					<p className="text-text-dim text-xs">Drop or open a video to start.</p>
				) : (
					<div className="space-y-1 max-h-48 overflow-y-auto">
						{timeline.map((clip: any, idx: number) => (
							<div key={clip.id}
								draggable
								onDragStart={() => { dragSrcRef.current = idx; }}
								onDragOver={(e: React.DragEvent) => { e.preventDefault(); e.dataTransfer.dropEffect = "move"; }}
								onDragEnter={(e: React.DragEvent) => {
									e.preventDefault();
									if (dragSrcRef.current !== null && dragSrcRef.current !== idx) {
										(e.currentTarget as HTMLElement).style.borderTopColor = "#D4F000";
										(e.currentTarget as HTMLElement).style.borderTopWidth = "2px";
									}
								}}
								onDragLeave={(e: React.DragEvent) => { (e.currentTarget as HTMLElement).style.borderTopColor = ""; (e.currentTarget as HTMLElement).style.borderTopWidth = ""; }}
								onDrop={(e: React.DragEvent) => {
									e.preventDefault();
									(e.currentTarget as HTMLElement).style.borderTopColor = "";
									(e.currentTarget as HTMLElement).style.borderTopWidth = "";
									const from = dragSrcRef.current;
									dragSrcRef.current = null;
									if (from !== null && from !== idx) {
										setTimeline((prev: any[]) => { const next = [...prev]; const [moved] = next.splice(from!, 1); next.splice(idx, 0, moved); return next; });
										setActiveIdx(idx);
									}
								}}
								onDragEnd={() => { dragSrcRef.current = null; }}
								className={`flex items-center gap-1 rounded px-2 py-1.5 text-xs cursor-pointer transition-colors ${
									idx === activeIdx ? "bg-y/10 border border-y/40" : "bg-muted border border-transparent hover:border-border"
								}`}
								onClick={() => selectClip(idx)}
							>
								<GripHorizontal size={11} className="text-text-dim flex-shrink-0 opacity-40 cursor-grab" />
								<span className="truncate flex-1 text-text-mid font-mono" title={clip.name}>{clip.name}</span>
								<span className="text-text-dim text-[10px] font-mono tabular-nums flex-shrink-0">{formatTime(clip.trimOut - clip.trimIn)}</span>
								<button onClick={(e: React.MouseEvent) => { e.stopPropagation(); moveClip(idx, -1); }} disabled={idx === 0} className="p-0.5 text-text-dim hover:text-white disabled:opacity-20 flex-shrink-0"><ArrowUp size={10} /></button>
								<button onClick={(e: React.MouseEvent) => { e.stopPropagation(); moveClip(idx, 1); }} disabled={idx === timeline.length - 1} className="p-0.5 text-text-dim hover:text-white disabled:opacity-20 flex-shrink-0"><ArrowDown size={10} /></button>
								<button onClick={(e: React.MouseEvent) => { e.stopPropagation(); removeFromTimeline(idx); }} className="p-0.5 text-red-400 hover:text-red-300 flex-shrink-0"><Trash2 size={10} /></button>
							</div>
						))}
					</div>
				)}
			</div>

			<hr className="border-border" />

			{/* Active clip name */}
			{filePath && (
				<div className="space-y-1">
					<p className="label">Active Clip</p>
					{renaming ? (
						<div ref={renameRef} className="flex items-center gap-1">
							<input ref={renameInputRef} value={renameValue} onChange={(e) => setRenameValue(e.target.value)}
								onKeyDown={(e: React.KeyboardEvent) => { if (e.key === "Enter") handleRename(); if (e.key === "Escape") cancelRename(); }}
								className="bg-[#1a1a1a] border border-border rounded-lg px-2 py-1 text-xs text-white flex-1 min-w-0 outline-none focus:border-y transition-colors" autoFocus />
							<button onMouseDown={(e: React.MouseEvent) => e.preventDefault()} onClick={handleRename} className="p-1.5 rounded bg-y text-black hover:bg-yd transition-colors"><Check size={13} /></button>
						</div>
					) : (
						<div className="flex items-center gap-1 group">
							<span className="text-xs text-white truncate flex-1 font-mono">{timeline[activeIdx]?.name ?? filePath.replace(/^.*[\\/]/, "")}</span>
							<button onClick={() => { const entry = timeline[activeIdx]; const name = (entry?.name ?? "").replace(/\.[^.]+$/, ""); setRenameValue(name); setRenaming(true); setTimeout(() => renameInputRef.current?.select(), 50); }}
								className="p-1 text-text-dim hover:text-y border border-transparent hover:border-y rounded transition-colors"><Pen size={11} /></button>
						</div>
					)}
				</div>
			)}

			{/* Merge button */}
			{timeline.length > 1 && (
				<button onClick={handleQuickMerge} disabled={exporting}
					className="w-full flex items-center justify-center gap-2 py-2.5 rounded-lg border-2 border-y/40 bg-y/5 text-y text-xs font-bold hover:bg-y/10 transition-colors disabled:opacity-50">
					{exporting ? <><Loader2 size={14} className="animate-spin" /> Merging…</> : <><Scissors size={14} /> Merge {timeline.length} Clips</>}
				</button>
			)}

			{/* Trim info */}
			<Section title="Trim">
				<Row label="In" val={formatTime(trimIn)} />
				<Row label="Out" val={formatTime(trimOut)} />
				<Row label="Duration" val={formatTime(trimOut - trimIn)} />
			</Section>

			{/* Cuts */}
			{timeline.length === 1 && (
				<Section title="Cuts / Removed Segments">
					{cuts.length === 0 ? (
						<p className="text-text-dim text-xs">Right-click the timeline or press <kbd className="bg-muted px-1 rounded">X</kbd> to cut.</p>
					) : (
						<div className="space-y-1 max-h-40 overflow-y-auto">
							{cuts.map((c: any, i: number) => (
								<div key={i} className="flex items-center justify-between bg-muted rounded px-2 py-1 text-xs">
									<span className="text-text-mid font-mono">{formatTime(c.start)} – {formatTime(c.end)} <span className="text-text-dim ml-1">({formatTime(c.end - c.start)})</span></span>
									<button onClick={() => removeCut(i)} className="text-red-400 hover:text-red-300 flex-shrink-0 ml-1"><Trash2 size={12} /></button>
								</div>
							))}
						</div>
					)}
					{cuts.length > 0 && (
						<button onClick={() => setCuts([])} className="text-[11px] text-text-dim hover:text-white transition-colors flex items-center gap-1 mt-1"><RotateCcw size={10} /> Clear all cuts</button>
					)}
				</Section>
			)}

			{/* Export Presets */}
			<Section title="Export Presets">
				<div className="grid grid-cols-5 gap-1.5">
					{[
						{ label: "YT Shorts", res: "1080p", aspect: "9:16" },
						{ label: "TikTok", res: "1080p", aspect: "9:16" },
						{ label: "Reels", res: "1080p", aspect: "9:16" },
						{ label: "Square", res: "1080p", aspect: "1:1" },
						{ label: "Original", res: "source", aspect: "16:9" },
					].map((p) => {
						const active = expResolution === p.res && expAspect === p.aspect;
						return <button key={p.label} onClick={() => { setExpFormat("mp4"); setExpResolution(p.res); setExpAspect(p.aspect); }} className={`text-[11px] font-medium px-2 py-1.5 rounded border transition-colors text-center ${active ? "border-y text-y bg-y/10" : "border-border hover:border-y hover:text-y"}`}>{p.label}</button>;
					})}
				</div>
			</Section>

			<Section title="Format">
				<Select label="Container" value={expFormat} onChange={setExpFormat} options={["mp4", "webm", "mkv", "mov"]} />
			</Section>

			<Section title="Video">
				<Select label="Resolution" value={expResolution} onChange={setExpResolution} options={["source", "480p", "720p", "1080p", "1440p", "4k"]} />
				<Select label="Aspect Ratio" value={expAspect} onChange={setExpAspect} options={["16:9", "9:16", "1:1", "4:5", "4:3", "21:9"]} />
			</Section>

			<AspectPreview ratio={expAspect} />

			{exportDone && (
				<div className="bg-[#0a1a00] border border-[#2a4a00] rounded-lg p-3 text-xs text-[#aaff44]">
					✓ Exported:<br /><span className="text-text-mid break-all">{exportDone.split(/[\\/]/).pop()}</span>
					<button className="mt-1 text-y underline block" onClick={() => bridge.showInFolder(exportDone)}>Show in folder</button>
				</div>
			)}
			{exportError && (
				<div className="bg-red-900/30 border border-red-700 rounded-lg p-3 text-xs text-red-300 flex items-start gap-2">
					<span className="flex-1">⚠ {exportError}</span>
					<button onClick={() => setExportError(null)} className="text-red-400 hover:text-white flex-shrink-0 mt-0.5">✕</button>
				</div>
			)}

			{/* Video Adjustments */}
			{filePath && (
				<>
				{/* Undo/Redo toolbar */}
				<div className="flex items-center gap-2">
					<button onClick={undo} disabled={historyIdx <= 0} className="flex items-center gap-1 px-2 py-1 rounded text-[11px] font-semibold border border-border text-text-mid hover:border-y hover:text-y transition-colors disabled:opacity-30 disabled:cursor-not-allowed" title="Undo (Ctrl+Z)">
						<RotateCcw size={11} /> Undo
					</button>
					<button onClick={redo} disabled={historyIdx >= history.length - 1} className="flex items-center gap-1 px-2 py-1 rounded text-[11px] font-semibold border border-border text-text-mid hover:border-y hover:text-y transition-colors disabled:opacity-30 disabled:cursor-not-allowed" title="Redo (Ctrl+Y)">
						<RotateCcw size={11} className="scale-x-[-1]" /> Redo
					</button>
					<span className="text-[10px] text-text-dim ml-auto">{historyIdx + 1}/{history.length}</span>
				</div>

				<hr className="border-border" />

				{/* Speed Ramping */}
				<div className="space-y-2">
					<p className="text-[11px] text-text-dim font-semibold uppercase tracking-wider flex items-center gap-1">⚡ Speed Ramping</p>
					
					{/* Quick speed presets */}
					<div className="grid grid-cols-4 gap-1">
						{[0.25, 0.5, 1, 2].map((spd) => (
							<button
								key={spd}
								onClick={() => {
									const seg: SpeedSegment = {
										id: crypto.randomUUID(),
										start: currentTime,
										end: Math.min(currentTime + 3, duration),
										speed: spd,
									};
									setSpeedSegments((prev: SpeedSegment[]) => [...prev, seg].sort((a, b) => a.start - b.start));
									pushHistory();
								}}
								className={`text-[11px] font-bold px-1 py-1.5 rounded border transition-colors text-center ${
									spd === 1 ? "border-y/40 text-y bg-y/5" : "border-border text-text-mid hover:border-y hover:text-y"
								}`}
							>
								{spd}x
							</button>
						))}
					</div>

					<p className="text-[10px] text-text-dim">Add at playhead. Drag edges on timeline to adjust.</p>

					{/* Active speed segments */}
					{speedSegments.length > 0 && (
						<div className="space-y-1 max-h-32 overflow-y-auto">
							{speedSegments.map((seg: SpeedSegment, i: number) => (
								<div key={seg.id} className="flex items-center gap-2 px-2 py-1.5 rounded text-xs bg-muted border border-border group">
									<span className="text-y font-bold text-[11px] w-7">{seg.speed}x</span>
									<span className="text-text-dim font-mono text-[10px] flex-1">{formatTime(seg.start)} – {formatTime(seg.end)}</span>
									<select
										value={seg.speed}
										onChange={(e) => {
											setSpeedSegments((prev: SpeedSegment[]) => prev.map((s) => s.id === seg.id ? { ...s, speed: Number(e.target.value) } : s));
											pushHistory();
										}}
										className="bg-transparent text-[10px] text-text-mid border-0 outline-none w-12"
									>
										{[0.1, 0.25, 0.5, 0.75, 1, 1.5, 2, 3, 4].map((v) => <option key={v} value={v}>{v}x</option>)}
									</select>
									<button onClick={() => { setSpeedSegments((prev: SpeedSegment[]) => prev.filter((s) => s.id !== seg.id)); pushHistory(); }}
										className="text-red-400 hover:text-red-300 opacity-0 group-hover:opacity-100 transition-opacity">
										<Trash2 size={10} />
									</button>
								</div>
							))}
						</div>
					)}
					{speedSegments.length > 0 && (
						<button onClick={() => { setSpeedSegments([]); pushHistory(); }} className="text-[11px] text-text-dim hover:text-y flex items-center gap-1">
							<RotateCcw size={10} /> Clear speed ramps
						</button>
					)}
				</div>

				<hr className="border-border" />

				{/* Transitions */}
				<div className="space-y-2">
					<p className="text-[11px] text-text-dim font-semibold uppercase tracking-wider flex items-center gap-1">🎬 Transitions</p>
					
					{cuts.length === 0 ? (
						<p className="text-text-dim text-[11px]">Add cuts first, then apply transitions between them.</p>
					) : (
						<>
							<div className="grid grid-cols-3 gap-1.5">
								{([
									{ type: "crossfade" as TransitionType, label: "Crossfade", icon: "↔", hover: "trans-hover-crossfade" },
									{ type: "glitch" as TransitionType, label: "Glitch", icon: "⚡", hover: "trans-hover-glitch" },
									{ type: "whip-pan" as TransitionType, label: "Whip Pan", icon: "💨", hover: "trans-hover-whip-pan" },
									{ type: "flash" as TransitionType, label: "Flash", icon: "✦", hover: "trans-hover-flash" },
									{ type: "zoom-in" as TransitionType, label: "Zoom In", icon: "🔍", hover: "trans-hover-zoom-in" },
									{ type: "zoom-out" as TransitionType, label: "Zoom Out", icon: "🔎", hover: "trans-hover-zoom-out" },
								]).map((tr) => (
									<button
										key={tr.type}
										onClick={() => {
											// Add transition at each cut point
											const newTransitions: Transition[] = cuts.map((c: CutSeg) => ({
												id: crypto.randomUUID(),
												time: c.start,
												type: tr.type,
												duration: 0.5,
											}));
											setTransitions(newTransitions);
											pushHistory();
										}}
										className={`flex flex-col items-center gap-0.5 px-1 py-2 rounded border transition-all text-center ${tr.hover} ${
											transitions.length > 0 && transitions[0]?.type === tr.type
												? "border-y text-y bg-y/10 scale-[1.02]"
												: "border-border text-text-mid hover:border-y hover:text-y hover:scale-[1.02]"
										}`}
									>
										<span className="text-sm">{tr.icon}</span>
										<span className="text-[10px] font-medium">{tr.label}</span>
									</button>
								))}
							</div>

							{transitions.length > 0 && (
								<div className="space-y-1.5">
									<div className="flex items-center justify-between">
										<span className="text-[10px] text-text-dim">{transitions.length} transition{transitions.length !== 1 ? "s" : ""} applied</span>
										<button onClick={() => { setTransitions([]); pushHistory(); }} className="text-[10px] text-text-dim hover:text-y">Clear</button>
									</div>
									<label className="flex items-center justify-between text-[11px] text-text-dim">
										<span>Duration</span>
										<span className="text-white font-mono">{transitions[0]?.duration.toFixed(1)}s</span>
									</label>
									<input
										type="range" min={0.2} max={1.5} step={0.1}
										value={transitions[0]?.duration ?? 0.5}
										onChange={(e) => {
											const dur = Number(e.target.value);
											setTransitions((prev: Transition[]) => prev.map((t) => ({ ...t, duration: dur })));
											pushHistory();
										}}
										className="w-full accent-y h-1"
									/>
								</div>
							)}
						</>
					)}
				</div>

				<hr className="border-border" />

				<div className="space-y-2">
					<p className="text-[11px] text-text-dim font-semibold uppercase tracking-wider">Adjustments</p>
					<div className="space-y-2">
						<label className="flex items-center justify-between text-xs text-text-dim">
							<span>Brightness</span><span className="text-white font-mono">{brightness}%</span>
						</label>
						<input type="range" min="50" max="200" value={brightness} onChange={(e) => setBrightness(Number(e.target.value))} className="w-full accent-y h-1" />
						<label className="flex items-center justify-between text-xs text-text-dim">
							<span>Contrast</span><span className="text-white font-mono">{contrast}%</span>
						</label>
						<input type="range" min="50" max="200" value={contrast} onChange={(e) => setContrast(Number(e.target.value))} className="w-full accent-y h-1" />
						<label className="flex items-center justify-between text-xs text-text-dim">
							<span>Saturation</span><span className="text-white font-mono">{saturation}%</span>
						</label>
						<input type="range" min="0" max="200" value={saturation} onChange={(e) => setSaturation(Number(e.target.value))} className="w-full accent-y h-1" />
						{(brightness !== 100 || contrast !== 100 || saturation !== 100) && (
							<button onClick={() => { setBrightness(100); setContrast(100); setSaturation(100); }} className="text-[11px] text-text-dim hover:text-y">Reset</button>
						)}
					</div>
				</div>
				</>
			)}

			{/* Cloud upload */}
			{cloud.paired && filePath && (
				<>
					<button
						onClick={async () => {
							if (uploadBusy) return;
							if (uploadJob?.status === "failed") { cloud.retryJob(uploadJob.id); return; }
							const entry = timeline[activeIdx];
							const name = entry?.name ?? filePath.replace(/^.*[\\/]/, "");
							const stat = await bridge.getFileStats(filePath).catch(() => null);
							cloud.addToQueue(filePath, name, stat?.size ?? 0, {
								trimStart: trimIn > 0 ? trimIn : undefined,
								trimEnd: trimOut < duration ? trimOut : undefined,
								cuts: cuts.length > 0 ? cuts : undefined,
							});
						}}
						disabled={uploadBusy}
						className={`btn-ghost justify-center w-full py-2 ${uploadBusy ? "opacity-50" : ""} ${showUploaded ? "text-green-400" : ""} ${uploadJob?.status === "failed" ? "text-red-400" : ""}`}
					>
						{uploadBusy && <><Loader2 size={14} className="animate-spin" /> {uploadJob!.status === "queued" ? "Queued…" : "Uploading…"}</>}
						{showUploaded && !uploadBusy && <><Upload size={14} /> Uploaded</>}
						{!uploadBusy && !showUploaded && <><Upload size={14} /> Upload to Cloud</>}
					</button>
					{uploadJob?.status === "failed" && uploadJob?.error && (
						<div className="bg-red-900/30 border border-red-700 rounded-lg p-3 text-xs text-red-300 mt-1 flex items-start gap-2">
							<span className="flex-1">Upload failed: {uploadJob.error}</span>
							<button onClick={() => cloud.removeJob(uploadJob.id)} className="text-red-400 hover:text-white flex-shrink-0 mt-0.5">✕</button>
						</div>
					)}
				</>
			)}
			</div>
			{/* Sticky export button — always visible */}
			<div className="flex-shrink-0 p-3 border-t border-border space-y-2">
				{exporting && (
					<div className="space-y-1">
						<div className="flex items-center justify-between text-[11px]">
							<span className="text-text-dim">Exporting...</span>
							<span className="text-y font-bold font-mono">{exportProgress}%</span>
						</div>
						<div className="w-full h-2 bg-muted rounded-full overflow-hidden">
							<div className="h-full bg-y rounded-full transition-all duration-300 ease-out" style={{ width: `${exportProgress}%` }} />
						</div>
					</div>
				)}
				<button onClick={handleExport} disabled={exporting} className="btn-y justify-center w-full py-3 disabled:opacity-50">
					{exporting ? <><Loader2 size={16} className="animate-spin" /> Exporting… {exportProgress}%</> : <><Download size={16} /> Export Clip</>}
				</button>
			</div>
		</div>
	);
}


// ── Helper Components ───────────────────────────────────────────────────────
function VolumeSync({ videoRef, muted, volume }: { videoRef: React.RefObject<HTMLVideoElement | null>; muted: boolean; volume: number }) {
	useEffect(() => {
		const v = videoRef.current;
		if (v) { v.muted = muted; v.volume = volume; }
	}, [muted, volume, videoRef]);
	return null;
}

function TrimHandle({ position, onDrag, duration }: { position: number; onDrag: (t: number) => void; duration: number }) {
	return (
		<div
			className="absolute top-0 h-full w-6 -translate-x-1/2 flex items-center cursor-ew-resize group/handle z-20"
			style={{ left: `${position}%` }}
			onMouseDown={(e) => {
				e.stopPropagation();
				const parent = e.currentTarget.parentElement!;
				const move = (me: MouseEvent) => {
					const rect = parent.getBoundingClientRect();
					const t = ((me.clientX - rect.left) / rect.width) * duration;
					onDrag(t);
				};
				const up = () => { window.removeEventListener("mousemove", move); window.removeEventListener("mouseup", up); };
				window.addEventListener("mousemove", move);
				window.addEventListener("mouseup", up);
			}}
		>
			<div className="w-2 h-full bg-y group-hover/handle:shadow-[0_0_12px_#D4F000]" />
		</div>
	);
}

function SpeedEdgeHandle({ position, side, onDrag, onDragEnd, duration }: {
	position: number; side: "left" | "right"; onDrag: (t: number) => void; onDragEnd: () => void; duration: number;
}) {
	return (
		<div
			className="absolute top-0 h-full w-4 -translate-x-1/2 flex items-end cursor-ew-resize z-[8] group/speed-edge"
			style={{ left: `${position}%` }}
			onMouseDown={(e) => {
				e.stopPropagation();
				const parent = e.currentTarget.parentElement!;
				const move = (me: MouseEvent) => {
					const rect = parent.getBoundingClientRect();
					const t = ((me.clientX - rect.left) / rect.width) * duration;
					onDrag(t);
				};
				const up = () => {
					window.removeEventListener("mousemove", move);
					window.removeEventListener("mouseup", up);
					onDragEnd();
				};
				window.addEventListener("mousemove", move);
				window.addEventListener("mouseup", up);
			}}
		>
			<div className={`w-1 h-6 rounded-full bg-blue-400 group-hover/speed-edge:bg-blue-300 group-hover/speed-edge:shadow-[0_0_8px_#3b82f6] transition-all ${side === "left" ? "ml-auto" : "mr-auto"}`} />
		</div>
	);
}

function ThumbnailStrip({ videoRef, duration, filePath }: { videoRef: React.RefObject<HTMLVideoElement | null>; duration: number; filePath: string | null }) {
	const canvasRef = useRef<HTMLCanvasElement>(null);
	const [thumbnails, setThumbnails] = useState<string[]>([]);
	const extractingRef = useRef(false);
	const lastPathRef = useRef<string | null>(null);

	useEffect(() => {
		if (!filePath || !duration || duration <= 0 || extractingRef.current) return;
		if (filePath === lastPathRef.current && thumbnails.length > 0) return;
		lastPathRef.current = filePath;
		extractingRef.current = true;

		// Create a hidden video element for frame extraction (don't disturb playback)
		const extractVideo = document.createElement("video");
		extractVideo.src = videoRef.current?.src || "";
		extractVideo.crossOrigin = "anonymous";
		extractVideo.muted = true;
		extractVideo.preload = "auto";

		const canvas = document.createElement("canvas");
		const ctx = canvas.getContext("2d");
		if (!ctx) { extractingRef.current = false; return; }

		const thumbWidth = 80;
		const thumbHeight = 45;
		canvas.width = thumbWidth;
		canvas.height = thumbHeight;

		const interval = Math.max(2, Math.floor(duration / 20)); // ~20 thumbnails max, at least every 2s
		const times: number[] = [];
		for (let t = 0; t < duration; t += interval) times.push(t);

		const results: string[] = [];
		let idx = 0;

		const extractNext = () => {
			if (idx >= times.length) {
				setThumbnails(results);
				extractingRef.current = false;
				extractVideo.src = ""; extractVideo.load(); extractVideo.remove();
				return;
			}
			extractVideo.currentTime = times[idx];
		};

		extractVideo.onseeked = () => {
			ctx.drawImage(extractVideo, 0, 0, thumbWidth, thumbHeight);
			results.push(canvas.toDataURL("image/jpeg", 0.5));
			idx++;
			extractNext();
		};

		extractVideo.onloadeddata = () => extractNext();
		extractVideo.onerror = () => { extractingRef.current = false; extractVideo.src = ""; extractVideo.load(); };

		return () => { extractVideo.src = ""; extractVideo.load(); extractVideo.remove(); };
	}, [filePath, duration]);

	if (thumbnails.length === 0 || !duration) return null;

	return (
		<div className="flex h-14 rounded overflow-hidden bg-black/40 border border-border/50">
			{thumbnails.map((src, i) => (
				<img key={i} src={src} alt="" className="h-full object-cover flex-1 min-w-0 border-r border-black/30 last:border-r-0" draggable={false} />
			))}
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

function Select({ label, value, onChange, options }: { label: string; value: string; onChange: (v: string) => void; options: string[] }) {
	return (
		<div className="space-y-1">
			<p className="text-text-dim text-[11px]">{label}</p>
			<select value={value} onChange={(e) => onChange(e.target.value)} className="input text-xs py-1.5 no-drag">
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
	const map: Record<string, string> = { "16:9": "w-20 h-[45px]", "9:16": "w-10 h-[71px]", "4:3": "w-16 h-12", "21:9": "w-24 h-[41px]" };
	const cls = map[ratio] ?? "w-16 h-9";
	return (
		<div className="flex flex-col items-center gap-1">
			<p className="label">Preview</p>
			<div className={`${cls} bg-muted border border-y rounded flex items-center justify-center`}><Crop size={14} className="text-y" /></div>
			<p className="text-text-dim text-xs">{ratio}</p>
		</div>
	);
}
