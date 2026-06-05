import { useState } from "react";
import { Save, FolderOpen, RotateCcw, Keyboard, Monitor, Volume2, Cpu, HardDrive, Cloud, Upload } from "lucide-react";
import type { AppSettings } from "../../types";
import { DEFAULTS } from "../../hooks/useSettings";
import type { useCloudUpload } from "../../hooks/useCloudUpload";

interface Props {
	settings: AppSettings;
	updateSetting: <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => void;
	saveAll: (s: AppSettings) => Promise<void>;
	cloud: ReturnType<typeof useCloudUpload>;
}

export default function SettingsPage({ settings, updateSetting, saveAll, cloud }: Props) {
	const [local, setLocal] = useState<AppSettings>({ ...settings });
	const [saved, setSaved] = useState(false);

	const update = <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => {
		setLocal((prev) => ({ ...prev, [key]: value }));
	};

	const handleSave = async () => {
		await saveAll(local);
		setSaved(true);
		setTimeout(() => setSaved(false), 2500);
	};

	const browseFolder = async () => {
		const folder = await window.clipsta?.browseFolder();
		if (folder) update("outputFolder", folder);
	};

	return (
		<div className="h-full overflow-y-auto p-6 max-w-3xl mx-auto">
			<div className="flex items-center justify-between mb-6">
				<div>
					<h1 className="text-2xl font-black text-white">Settings</h1>
					<p className="text-text-dim text-sm mt-0.5">Configure recording, hotkeys and export</p>
				</div>
				<div className="flex items-center gap-3">
					<button onClick={() => setLocal({ ...DEFAULTS })} className="btn-ghost">
						<RotateCcw size={14} /> Reset Defaults
					</button>
					<button onClick={handleSave} className="btn-y">
						<Save size={14} />
						{saved ? "Saved ✓" : "Save Settings"}
					</button>
				</div>
			</div>

			<div className="space-y-6">

				{/* ── Hotkeys ── */}
				<Section icon={<Keyboard size={16} />} title="Hotkeys">
					<p className="text-text-dim text-xs mb-3">
						These work globally even when the app is minimized. Use modifiers like Alt, Ctrl, Shift.
					</p>
					<div className="grid grid-cols-3 gap-4">
						<HotkeyField label="Start / Stop Recording" value={local.hotkeyRecord}
							onChange={(v) => update("hotkeyRecord", v)} />
						<HotkeyField label="Save Last 1 Minute" value={local.hotkeyClip1Min}
							onChange={(v) => update("hotkeyClip1Min", v)} />
						<HotkeyField label="Save Last 5 Minutes" value={local.hotkeyClip5Min}
							onChange={(v) => update("hotkeyClip5Min", v)} />
					</div>
					<p className="text-text-dim text-[10px] mt-2">
						Examples: F9, Alt+F9, Ctrl+Shift+R. The 1-min and 5-min keys save from the rolling buffer while recording.
					</p>
				</Section>

				{/* ── Video ── */}
				<Section icon={<Monitor size={16} />} title="Video">
					<div className="grid grid-cols-2 gap-4">
						<SelectField label="Resolution" value={local.resolution}
							onChange={(v) => update("resolution", v)}
							options={["480p", "720p", "1080p", "1440p", "4k"]} />
						<SelectField label="Frame Rate" value={String(local.fps)}
							onChange={(v) => update("fps", Number(v))}
							options={["30", "60", "120"]} />
						<SelectField label="Encoder" value={local.encoder}
							onChange={(v) => update("encoder", v)}
							options={["auto", "x264 (Software)", "NVENC (NVIDIA)", "AMF (AMD)", "QuickSync (Intel)"]} />
						<SelectField label="Aspect Ratio" value={local.aspectRatio}
							onChange={(v) => update("aspectRatio", v)}
							options={["16:9", "9:16", "1:1", "4:3", "21:9"]} />
						<NumberField label="Video Bitrate (kbps)" value={local.bitrate}
							onChange={(v) => update("bitrate", v)} min={1000} max={50000} step={500} />
						<SelectField label="Buffer Duration" value={String(local.bufferDuration)}
							onChange={(v) => update("bufferDuration", Number(v))}
							options={["30", "60", "120", "180", "300"]}
							display={(v) => v === "60" ? "1 minute" : v === "300" ? "5 minutes" : `${v}s`} />
					</div>
				</Section>

				{/* ── Audio ── */}
				<Section icon={<Volume2 size={16} />} title="Audio">
					<div className="grid grid-cols-2 gap-4">
						<SelectField label="Audio Source" value={local.audioSource}
							onChange={(v) => update("audioSource", v)}
							options={["desktop", "mic", "both", "none"]}
							display={(v) =>
								v === "desktop" ? "Desktop Audio (system)" :
								v === "mic" ? "Microphone only" :
								v === "both" ? "Desktop + Microphone" : "None"} />
						<NumberField label="Audio Bitrate (kbps)" value={local.audioBitrate}
							onChange={(v) => update("audioBitrate", v)} min={64} max={320} step={32} />
					</div>
				</Section>

				{/* ── Capture ── */}
				<Section icon={<Cpu size={16} />} title="Capture Behaviour">
					<div className="grid grid-cols-2 gap-4">
						<Toggle label="Auto Game Detection" checked={local.gameDetect}
							onChange={(v) => update("gameDetect", v)}
							description="Automatically capture the active fullscreen game" />
						<Toggle label="Minimize to System Tray" checked={local.minimizeToTray}
							onChange={(v) => update("minimizeToTray", v)}
							description="Keep running in tray when window is closed" />
						<Toggle label="Show Overlay (OBS-style)" checked={local.overlayEnabled}
							onChange={(v) => update("overlayEnabled", v)}
							description="Recording indicator while capturing" />
					</div>
				</Section>

				{/* ── Cloud ── */}
				<Section icon={<Cloud size={16} />} title="Cloud Upload">
					<div className="grid grid-cols-2 gap-4">
						<Toggle label="Enable Cloud Upload" checked={local.cloudEnabled}
							onChange={(v) => update("cloudEnabled", v)}
							description="Upload clips to your paired mobile device" />
						<Toggle label="Auto-Upload New Clips" checked={local.autoUpload}
							onChange={(v) => update("autoUpload", v)}
							description="Automatically queue new recordings for upload" />
						<NumberField label="Upload Bandwidth (KB/s)" value={local.uploadBandwidth}
							onChange={(v) => update("uploadBandwidth", v)} min={0} max={50000} step={500} />
						<Toggle label="Delete After Upload" checked={local.deleteAfterUpload}
							onChange={(v) => update("deleteAfterUpload", v)}
							description="Remove local clip file after successful cloud upload" />
					</div>
					{local.cloudEnabled && (
						<div className="bg-muted rounded-lg p-3 space-y-2">
							<div className="flex items-center justify-between">
								<p className="text-xs text-text-dim font-semibold">Pairing</p>
								{cloud.paired ? (
									<span className="text-[10px] text-green-400 font-bold">✓ Paired</span>
								) : (
									<span className="text-[10px] text-text-dim">Not paired</span>
								)}
							</div>
							{cloud.pairingCode && (
								<div className="text-center py-2">
									<p className="text-[10px] text-text-dim mb-1">Enter this code on your mobile app:</p>
									<p className="text-2xl font-black text-y tracking-widest">{cloud.pairingCode}</p>
								</div>
							)}
							{!cloud.paired && (
								<button
									onClick={cloud.generatePairingCode}
									className="btn-y w-full justify-center text-xs py-2"
									disabled={cloud.pairingError !== null}
								>
									<Upload size={12} /> Generate Pairing Code
								</button>
							)}
							{cloud.pairingError && (
								<p className="text-red-400 text-[10px]">{cloud.pairingError}</p>
							)}
							{local.uploadBandwidth === 0 && (
								<p className="text-[10px] text-text-dim">Bandwidth: 0 = unlimited</p>
							)}
						</div>
					)}
				</Section>

				{/* ── Output ── */}
				<Section icon={<HardDrive size={16} />} title="Output">
					<div className="space-y-1">
						<p className="label">Clips Folder</p>
						<div className="flex gap-2">
							<input
								value={local.outputFolder}
								onChange={(e) => update("outputFolder", e.target.value)}
								className="input flex-1"
								placeholder="C:\Users\...\Videos\Clipsta"
							/>
							<button onClick={browseFolder} className="btn-ghost flex-shrink-0">
								<FolderOpen size={14} /> Browse
							</button>
							<button
								onClick={() => window.clipsta?.openFolder(local.outputFolder)}
								className="btn-ghost flex-shrink-0"
							>
								Open
							</button>
						</div>
					</div>
				</Section>

			</div>

			{/* Save bar */}
			<div className="sticky bottom-0 pt-6 pb-2 bg-gradient-to-t from-bg to-transparent">
				<button onClick={handleSave} className="btn-y w-full justify-center py-3 text-base">
					<Save size={18} />
					{saved ? "Settings Saved ✓" : "Save All Settings"}
				</button>
			</div>
		</div>
	);
}

function Section({ icon, title, children }: { icon: React.ReactNode; title: string; children: React.ReactNode }) {
	return (
		<div className="card p-5 space-y-4">
			<div className="flex items-center gap-2 border-b border-border pb-3">
				<span className="text-y">{icon}</span>
				<h3 className="font-bold text-white">{title}</h3>
			</div>
			{children}
		</div>
	);
}

function HotkeyField({ label, value, onChange }: { label: string; value: string; onChange: (v: string) => void }) {
	const [capturing, setCapturing] = useState(false);
	const [keys, setKeys] = useState<string[]>([]);

	const startCapture = () => {
		setCapturing(true);
		setKeys([]);
	};

	const onKeyDown = (e: React.KeyboardEvent) => {
		e.preventDefault();
		const parts: string[] = [];
		if (e.ctrlKey) parts.push("Ctrl");
		if (e.altKey) parts.push("Alt");
		if (e.shiftKey) parts.push("Shift");
		if (!["Control", "Alt", "Shift", "Meta"].includes(e.key)) {
			parts.push(e.key.length === 1 ? e.key.toUpperCase() : e.key);
		}
		setKeys(parts);
		if (!["Control", "Alt", "Shift", "Meta"].includes(e.key)) {
			onChange(parts.join("+"));
			setCapturing(false);
		}
	};

	return (
		<div className="space-y-1">
			<p className="label">{label}</p>
			{capturing ? (
				<input
					autoFocus
					className="input font-mono text-y text-center"
					placeholder="Press keys…"
					value={keys.join("+")}
					onKeyDown={onKeyDown}
					onBlur={() => setCapturing(false)}
					readOnly
				/>
			) : (
				<button
					onClick={startCapture}
					className="input font-mono text-y text-center cursor-pointer hover:border-y w-full"
				>
					{value || "Click to set"}
				</button>
			)}
		</div>
	);
}

function SelectField({ label, value, onChange, options, display }: {
	label: string; value: string; onChange: (v: string) => void;
	options: string[]; display?: (v: string) => string;
}) {
	return (
		<div className="space-y-1">
			<p className="label">{label}</p>
			<select value={value} onChange={(e) => onChange(e.target.value)} className="input no-drag">
				{options.map((o) => <option key={o} value={o}>{display ? display(o) : o}</option>)}
			</select>
		</div>
	);
}

function NumberField({ label, value, onChange, min, max, step }: {
	label: string; value: number; onChange: (v: number) => void; min: number; max: number; step: number;
}) {
	return (
		<div className="space-y-1">
			<p className="label">{label}</p>
			<div className="flex items-center gap-2">
				<input
					type="range" min={min} max={max} step={step} value={value}
					onChange={(e) => onChange(Number(e.target.value))}
					className="flex-1 accent-[#D4F000] no-drag"
				/>
				<span className="text-white text-sm font-mono w-16 text-right">{value.toLocaleString()}</span>
			</div>
		</div>
	);
}

function Toggle({ label, checked, onChange, description }: {
	label: string; checked: boolean; onChange: (v: boolean) => void; description?: string;
}) {
	return (
		<label className="flex items-start gap-3 cursor-pointer group">
			<div
				onClick={() => onChange(!checked)}
				className={`mt-0.5 w-10 h-5 rounded-full flex-shrink-0 transition-colors relative cursor-pointer
					${checked ? "bg-y" : "bg-muted"}`}
			>
				<div className={`absolute top-0.5 w-4 h-4 rounded-full bg-black transition-transform
					${checked ? "translate-x-5" : "translate-x-0.5"}`} />
			</div>
			<div>
				<p className="text-sm text-white font-medium group-hover:text-y transition-colors">{label}</p>
				{description && <p className="text-text-dim text-xs mt-0.5">{description}</p>}
			</div>
		</label>
	);
}
