export function toFileUrl(p: string): string {
	if (p.startsWith("file://")) return p;
	const normalized = p.replace(/\\/g, "/");
	const cleaned = normalized.startsWith("/") ? normalized.slice(1) : normalized;
	return `file:///${cleaned.replace(/#/g, "%23").replace(/\?/g, "%3F")}`;
}

export function formatTime(s: number) {
	if (!isFinite(s)) return "0:00";
	const sign = s < 0 ? "-" : "";
	const abs = Math.abs(s);
	const m = Math.floor(abs / 60);
	const sec = Math.floor(abs % 60);
	const ms = Math.floor((abs % 1) * 10);
	return `${sign}${m}:${String(sec).padStart(2, "0")}.${ms}`;
}

export function sanitizeName(s: string) {
	return s.replace(/[<>:"/\\|?*]/g, "").trim() || "clip";
}

export function getDroppedPaths(dt: DataTransfer): string[] {
	const paths: string[] = [];
	for (const f of Array.from(dt.files)) {
		const p = (f as any).path || f.name;
		if (p && /\.(webm|mp4|mkv|mov)$/i.test(p)) paths.push(p);
	}
	return paths;
}

export function pct(t: number, duration: number) {
	return duration > 0 ? (t / duration) * 100 : 0;
}

export function getTimeFromEvent(clientX: number, parentEl: Element, duration: number) {
	const rect = parentEl.getBoundingClientRect();
	return ((clientX - rect.left) / rect.width) * duration;
}

export function getFileExtension(filename: string) {
	const m = filename.match(/\.([^.]+)$/);
	return m ? m[1].toLowerCase() : "";
}

export function getVideoMime(filename: string) {
	const ext = getFileExtension(filename);
	switch (ext) {
		case "webm": return "video/webm";
		case "mkv": return "video/x-matroska";
		case "mov": return "video/quicktime";
		default: return "video/mp4";
	}
}
