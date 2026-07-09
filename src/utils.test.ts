import { describe, it, expect } from "vitest";
import { toFileUrl, formatTime, sanitizeName, getDroppedPaths, pct, getTimeFromEvent, getFileExtension, getVideoMime } from "./utils";

// ── toFileUrl ──────────────────────────────────────────────────────────────
describe("toFileUrl", () => {
	it("passes file:// URIs through unchanged", () => {
		expect(toFileUrl("file:///C:/videos/clip.mp4")).toBe("file:///C:/videos/clip.mp4");
	});

	it("converts a Windows path with backslashes", () => {
		const result = toFileUrl("C:\\Users\\test\\clip.mp4");
		expect(result).toBe("file:///C:/Users/test/clip.mp4");
	});

	it("converts a Unix path", () => {
		const result = toFileUrl("/home/user/clip.mp4");
		expect(result).toBe("file:///home/user/clip.mp4");
	});

	it("encodes # in the path", () => {
		const result = toFileUrl("C:\\videos\\clip#1.mp4");
		expect(result).toContain("%23");
		expect(result).not.toContain("#");
	});

	it("encodes ? in the path", () => {
		const result = toFileUrl("C:\\videos\\clip?test.mp4");
		expect(result).toContain("%3F");
		expect(result).not.toContain("?");
	});

	it("handles a path with no directory", () => {
		const result = toFileUrl("clip.mp4");
		expect(result).toBe("file:///clip.mp4");
	});
});

// ── formatTime ─────────────────────────────────────────────────────────────
describe("formatTime", () => {
	it("formats zero", () => {
		expect(formatTime(0)).toBe("0:00.0");
	});

	it("formats seconds only", () => {
		expect(formatTime(5)).toBe("0:05.0");
	});

	it("formats minutes and seconds", () => {
		expect(formatTime(125)).toBe("2:05.0");
	});

	it("shows tenths of seconds", () => {
		expect(formatTime(3.7)).toBe("0:03.7");
	});

	it("returns 0:00 for Infinity", () => {
		expect(formatTime(Infinity)).toBe("0:00");
	});

	it("returns 0:00 for NaN", () => {
		expect(formatTime(NaN)).toBe("0:00");
	});

	it("handles negative values", () => {
		expect(formatTime(-5)).toBe("-0:05.0");
	});
});

// ── sanitizeName ──────────────────────────────────────────────────────────
describe("sanitizeName", () => {
	it("passes through a clean name", () => {
		expect(sanitizeName("my_clip.mp4")).toBe("my_clip.mp4");
	});

	it("removes angle brackets", () => {
		expect(sanitizeName("clip<1>.mp4")).toBe("clip1.mp4");
	});

	it("removes colons and slashes", () => {
		expect(sanitizeName("a:b/c.mp4")).toBe("abc.mp4");
	});

	it("removes Windows-forbidden characters", () => {
		expect(sanitizeName('c"l:i<p>? clip.mp4')).toBe("clip clip.mp4");
	});

	it("returns 'clip' for empty result after stripping", () => {
		expect(sanitizeName("<>:\"/\\|?*")).toBe("clip");
	});

	it("trims whitespace", () => {
		expect(sanitizeName("  clip.mp4  ")).toBe("clip.mp4");
	});
});

// ── getDroppedPaths ───────────────────────────────────────────────────────
describe("getDroppedPaths", () => {
	function mockDataTransfer(files: { path?: string; name: string }[]): DataTransfer {
		return {
			files: files.map((f) => ({ path: f.path, name: f.name, size: 0, type: "" })) as any,
		} as DataTransfer;
	}

	it("accepts .mp4 files", () => {
		const dt = mockDataTransfer([{ path: "C:\\videos\\clip.mp4", name: "clip.mp4" }]);
		expect(getDroppedPaths(dt)).toEqual(["C:\\videos\\clip.mp4"]);
	});

	it("accepts .webm, .mkv, .mov files", () => {
		const dt = mockDataTransfer([
			{ path: "a.webm", name: "a.webm" },
			{ path: "b.mkv", name: "b.mkv" },
			{ path: "c.mov", name: "c.mov" },
		]);
		expect(getDroppedPaths(dt)).toHaveLength(3);
	});

	it("rejects non-video files", () => {
		const dt = mockDataTransfer([
			{ path: "clip.mp4", name: "clip.mp4" },
			{ path: "image.png", name: "image.png" },
			{ path: "doc.txt", name: "doc.txt" },
		]);
		expect(getDroppedPaths(dt)).toEqual(["clip.mp4"]);
	});

	it("uses .name when .path is not available", () => {
		const dt = mockDataTransfer([{ name: "clip.mp4" }]);
		expect(getDroppedPaths(dt)).toEqual(["clip.mp4"]);
	});

	it("ignores items without a name or path", () => {
		const dt = mockDataTransfer([{ name: "clip.mp4" }, { name: "" }]);
		expect(getDroppedPaths(dt)).toEqual(["clip.mp4"]);
	});
});

// ── pct ────────────────────────────────────────────────────────────────────
describe("pct", () => {
	it("returns 0 when duration is 0", () => {
		expect(pct(50, 0)).toBe(0);
	});

	it("calculates percentage correctly", () => {
		expect(pct(25, 100)).toBe(25);
	});

	it("returns 100 for full duration", () => {
		expect(pct(200, 200)).toBe(100);
	});

	it("handles fractional values", () => {
		expect(pct(1, 3)).toBeCloseTo(33.33, 1);
	});
});

// ── getTimeFromEvent ──────────────────────────────────────────────────────
describe("getTimeFromEvent", () => {
	function mockEl(width: number): Element {
		return { getBoundingClientRect: () => ({ left: 0, width } as DOMRect) } as Element;
	}

	it("converts pixel position to time", () => {
		const el = mockEl(200);
		expect(getTimeFromEvent(50, el, 100)).toBe(25);
	});

	it("handles right edge", () => {
		const el = mockEl(200);
		expect(getTimeFromEvent(200, el, 100)).toBe(100);
	});
});

// ── getFileExtension ──────────────────────────────────────────────────────
describe("getFileExtension", () => {
	it("returns extension from .mp4", () => {
		expect(getFileExtension("clip.mp4")).toBe("mp4");
	});

	it("returns extension from .WEBM (case insensitive)", () => {
		expect(getFileExtension("clip.WEBM")).toBe("webm");
	});

	it("returns empty string for no extension", () => {
		expect(getFileExtension("clip")).toBe("");
	});
});

// ── getVideoMime ──────────────────────────────────────────────────────────
describe("getVideoMime", () => {
	it("returns video/mp4 for .mp4", () => {
		expect(getVideoMime("clip.mp4")).toBe("video/mp4");
	});

	it("returns video/webm for .webm", () => {
		expect(getVideoMime("clip.webm")).toBe("video/webm");
	});

	it("returns video/mp4 for unknown extension", () => {
		expect(getVideoMime("clip.avi")).toBe("video/mp4");
	});
});
