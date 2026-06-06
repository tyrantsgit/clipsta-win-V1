import { useCallback, useEffect, useRef, useState } from "react";
import { Film, FolderOpen, Trash2, Scissors, Play, RefreshCw, Search, Upload, Loader2, CheckCircle, XCircle, Plus } from "lucide-react";
import type { ClipFile, UploadJob } from "../../types";
import type { useCloudUpload } from "../../hooks/useCloudUpload";

export default function LibraryPage({ onOpenEditor, cloud }: { onOpenEditor: (path: string) => void; cloud: ReturnType<typeof useCloudUpload> }) {
	const [clips, setClips] = useState<ClipFile[]>([]);
	const [loading, setLoading] = useState(true);
	const [selected, setSelected] = useState<ClipFile | null>(null);
	const [search, setSearch] = useState("");
	const [playing, setPlaying] = useState<string | null>(null);
	const videoRef = useRef<HTMLVideoElement>(null);

	const load = useCallback(async () => {
		setLoading(true);
		try {
			const list = await window.clipsta?.listClips() ?? [];
			setClips(list);
		} finally {
			setLoading(false);
		}
	}, []);

	useEffect(() => { load(); }, [load]);

	const addClip = async () => {
		const path = await window.clipsta?.browseFile();
		if (!path) return;
		const name = path.replace(/^.*[\\/]/, "");
		if (!confirm(`Add "${name}" to the library?`)) return;
		const destPath = await window.clipsta?.importClip(path);
		if (destPath && cloud.paired) {
			cloud.addToQueue(destPath, name, 0);
		}
		load();
	};

	const deleteClip = async (clip: ClipFile) => {
		if (!confirm(`Delete "${clip.name}"?`)) return;
		if (videoRef.current) {
			videoRef.current.pause();
			videoRef.current.removeAttribute("src");
			videoRef.current.load();
		}
		setPlaying(null);
		setSelected(null);
		await window.clipsta?.deleteClip(clip.path);
		load();
	};

	const playClip = (clip: ClipFile) => {
		setSelected(clip);
		setPlaying(clip.path);
		setTimeout(() => videoRef.current?.play(), 100);
	};

	const filtered = clips.filter((c) => c.name.toLowerCase().includes(search.toLowerCase()));

	const getUploadStatus = (clip: ClipFile): UploadJob | undefined => {
		return cloud.queue.find((j) => j.path === clip.path);
	};

	return (
		<div className="h-full flex overflow-hidden">
			<div className="w-72 flex-shrink-0 border-r border-border flex flex-col">
				<div className="p-4 border-b border-border space-y-3">
					<div className="flex items-center justify-between">
						<h2 className="font-bold text-white">Library</h2>
						<div className="flex items-center gap-2">
							<button onClick={load} className="text-text-dim hover:text-y transition-colors" title="Refresh">
								<RefreshCw size={14} className={loading ? "animate-spin" : ""} />
							</button>
							<button onClick={addClip} className="text-text-dim hover:text-y transition-colors" title="Add clip">
								<Plus size={14} />
							</button>
							<button
								onClick={() => window.clipsta?.openFolder("")}
								className="text-text-dim hover:text-y transition-colors"
								title="Open folder"
							>
								<FolderOpen size={14} />
							</button>
						</div>
					</div>
					<div className="relative">
						<Search size={13} className="absolute left-3 top-1/2 -translate-y-1/2 text-text-dim" />
						<input
							value={search}
							onChange={(e) => setSearch(e.target.value)}
							placeholder="Search clips…"
							className="input pl-8"
						/>
					</div>
					<p className="text-text-dim text-xs">{filtered.length} clip{filtered.length !== 1 ? "s" : ""}</p>
				</div>

				<div className="flex-1 overflow-y-auto p-2 space-y-1">
					{loading && (
						<div className="text-center py-10 text-text-dim text-sm">Loading…</div>
					)}
					{!loading && filtered.length === 0 && (
						<div className="text-center py-10">
							<Film size={36} className="mx-auto mb-3 text-text-dim opacity-40" />
							<p className="text-text-dim text-sm">No clips found</p>
							<p className="text-text-dim text-xs mt-1">Start recording to save clips</p>
						</div>
					)}
					{filtered.map((clip) => (
						<ClipRow
							key={clip.path}
							clip={clip}
							active={selected?.path === clip.path}
							onClick={() => setSelected(clip)}
							onPlay={() => playClip(clip)}
							onDelete={() => deleteClip(clip)}
							onEdit={() => onOpenEditor(clip.path)}
							onUpload={() => cloud.addToQueue(clip.path, clip.name, clip.size)}
							uploadStatus={cloud.queue.find((j) => j.path === clip.path)}
						/>
					))}
				</div>
			</div>

			{/* Viewer */}
			<div className="flex-1 flex flex-col overflow-hidden">
				{selected ? (
					<>
						<div className="flex-1 bg-black flex items-center justify-center overflow-hidden">
							<video
								ref={videoRef}
								src={playing === selected.path ? toFileUrl(selected.path) : undefined}
								className="max-w-full max-h-full"
								controls
								onEnded={() => setPlaying(null)}
							/>
						</div>
						<div className="p-4 border-t border-border flex items-center gap-3">
							<div className="flex-1 min-w-0">
								<p className="text-white font-semibold text-sm truncate">{selected.name}</p>
								<p className="text-text-dim text-xs">
									{formatSize(selected.size)} · {new Date(selected.createdAt).toLocaleString()}
								</p>
							</div>
							<div className="flex gap-2">
								{cloud.paired && (() => {
									const status = getUploadStatus(selected);
									if (status?.status === "done") {
										return <span className="text-green-400 text-[10px] flex items-center gap-1"><CheckCircle size={12} /> Uploaded</span>;
									}
									if (status?.status === "uploading") {
										return <span className="text-y text-[10px] flex items-center gap-1"><Loader2 size={12} className="animate-spin" /> {status.progress}%</span>;
									}
									if (status?.status === "queued") {
										return <span className="text-text-dim text-[10px] flex items-center gap-1"><Loader2 size={12} className="animate-spin" /> Queued</span>;
									}
									if (status?.status === "failed") {
										return (
											<>
												<span className="text-red-400 text-[10px] flex items-center gap-1"><XCircle size={12} /></span>
												<button onClick={() => cloud.retryJob(status.id)} className="btn-ghost text-[10px]">
													Retry
												</button>
											</>
										);
									}
									return (
										<button
											onClick={() => cloud.addToQueue(selected.path, selected.name, selected.size)}
											className="btn-ghost text-[10px]"
										>
											<Upload size={12} /> Upload
										</button>
									);
								})()}
								<button onClick={() => playClip(selected)} className="btn-ghost">
									<Play size={14} /> Play
								</button>
								<button onClick={() => onOpenEditor(selected.path)} className="btn-ghost">
									<Scissors size={14} /> Edit
								</button>
								<button onClick={() => window.clipsta?.showInFolder(selected.path)} className="btn-ghost">
									<FolderOpen size={14} /> Show
								</button>
								<button onClick={() => deleteClip(selected)} className="btn-danger">
									<Trash2 size={14} />
								</button>
							</div>
						</div>
					</>
				) : (
					<div className="flex-1 flex items-center justify-center text-center">
						<div>
							<Film size={64} className="mx-auto mb-4 text-text-dim opacity-20" />
							<p className="text-text-mid text-lg font-semibold">Select a clip to preview</p>
							<p className="text-text-dim text-sm mt-1">Your recorded clips appear in the list</p>
						</div>
					</div>
				)}
			</div>
		</div>
	);
}

const thumbCache = new Map<string, string>();
const thumbQueue = new Set<string>();

function useThumbnail(path: string): string | null {
	const [thumb, setThumb] = useState<string | null>(() => thumbCache.get(path) ?? null);

	useEffect(() => {
		if (!path) return;
		if (thumbCache.has(path)) {
			setThumb(thumbCache.get(path)!);
			return;
		}
		if (thumbQueue.has(path)) return;
		thumbQueue.add(path);

		let cancelled = false;
		const video = document.createElement("video");
		video.crossOrigin = "anonymous";
		video.muted = true;
		video.preload = "metadata";
		video.src = toFileUrl(path);

		video.onloadeddata = () => {
			if (cancelled) return;
			video.currentTime = 1;
		};

		video.onseeked = () => {
			if (cancelled) return;
			try {
				const canvas = document.createElement("canvas");
				canvas.width = 160;
				canvas.height = 90;
				const ctx = canvas.getContext("2d")!;
				ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
				const dataUrl = canvas.toDataURL("image/jpeg", 0.6);
				thumbCache.set(path, dataUrl);
				thumbQueue.delete(path);
				setThumb(dataUrl);
			} catch {
				thumbQueue.delete(path);
			}
			video.remove();
		};

		video.onerror = () => { thumbQueue.delete(path); video.remove(); };

		return () => {
			cancelled = true;
			video.remove();
		};
	}, [path]);

	return thumb;
}

function ClipRow({ clip, active, onClick, onPlay, onDelete, onEdit, onUpload, uploadStatus }: {
	clip: ClipFile; active: boolean;
	onClick: () => void; onPlay: () => void; onDelete: () => void; onEdit: () => void;
	onUpload?: () => void; uploadStatus?: UploadJob;
}) {
	const thumb = useThumbnail(clip.path);

	return (
		<div
			onClick={onClick}
			className={`rounded-lg p-2.5 cursor-pointer border transition-all group
				${active ? "border-y bg-[#1c1c00]" : "border-transparent hover:border-border hover:bg-card"}`}
		>
			<div className="flex items-center gap-2">
				<div className="w-14 h-9 bg-muted rounded flex-shrink-0 overflow-hidden flex items-center justify-center">
					{thumb ? (
						<img src={thumb} alt="" className="w-full h-full object-cover" />
					) : (
						<Film size={16} className="text-text-dim" />
					)}
				</div>
				<div className="flex-1 min-w-0">
					<p className={`text-xs font-semibold truncate ${active ? "text-y" : "text-white"}`}>
						{clip.name.replace(/\.[^.]+$/, "")}
					</p>
					<p className="text-[10px] text-text-dim">
						{formatSize(clip.size)} · {new Date(clip.createdAt).toLocaleDateString()}
					</p>
				</div>
				<div className="hidden group-hover:flex items-center gap-1">
					<button onClick={(e) => { e.stopPropagation(); onPlay(); }} className="p-1 hover:text-y text-text-dim transition-colors">
						<Play size={11} />
					</button>
					<button onClick={(e) => { e.stopPropagation(); onEdit(); }} className="p-1 hover:text-y text-text-dim transition-colors">
						<Scissors size={11} />
					</button>
					<button onClick={(e) => { e.stopPropagation(); onDelete(); }} className="p-1 hover:text-red-400 text-text-dim transition-colors">
						<Trash2 size={11} />
					</button>
					{onUpload && !uploadStatus && (
						<button onClick={(e) => { e.stopPropagation(); onUpload(); }} className="p-1 hover:text-y text-text-dim transition-colors" title="Upload to cloud">
							<Upload size={11} />
						</button>
					)}
					{uploadStatus?.status === "queued" && (
						<Loader2 size={11} className="text-text-dim animate-spin" />
					)}
					{uploadStatus?.status === "uploading" && (
						<Loader2 size={11} className="text-y animate-spin" />
					)}
					{uploadStatus?.status === "done" && (
						<CheckCircle size={11} className="text-green-400" />
					)}
					{uploadStatus?.status === "failed" && (
						<button onClick={(e) => { e.stopPropagation(); onUpload?.(); }} className="p-1 hover:text-y text-text-dim transition-colors" title="Retry upload">
							<Upload size={11} className="text-red-400" />
						</button>
					)}
				</div>
			</div>
		</div>
	);
}

function toFileUrl(p: string): string {
	if (p.startsWith("file://")) return p;
	const normalized = p.replace(/\\/g, "/");
	const cleaned = normalized.startsWith("/") ? normalized.slice(1) : normalized;
	return `file:///${cleaned.replace(/#/g, "%23").replace(/\?/g, "%3F")}`;
}

function formatSize(bytes: number) {
	if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
	return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
