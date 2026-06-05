import type { AppSettings } from "../types";
import type { RecorderState } from "../hooks/useRecorder";

export default function StatusBar({ recorder, settings }: {
	recorder: { state: RecorderState };
	settings: AppSettings;
}) {
	const { status, duration } = recorder.state;
	return (
		<div className="h-7 bg-[#080808] border-t border-border flex items-center px-4 gap-6 text-[11px] text-text-dim flex-shrink-0">
			<span className={status === "recording" ? "text-red-400 font-semibold" : ""}>
				{status === "recording" ? `⏺ Recording ${formatDur(duration)}` : "Ready"}
			</span>
			<span>Hotkeys: <span className="text-text-mid">{settings.hotkeyRecord}</span> rec · <span className="text-text-mid">{settings.hotkeyClip1Min}</span> 1-min · <span className="text-text-mid">{settings.hotkeyClip5Min}</span> 5-min</span>
			<span className="ml-auto">{settings.resolution} · {settings.fps}fps · {settings.encoder}</span>
		</div>
	);
}

function formatDur(s: number) {
	return `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;
}
