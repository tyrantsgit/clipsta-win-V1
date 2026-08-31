import { useEffect, useState } from "react";
import { Download, X, RefreshCw } from "lucide-react";
import bridge, { type UpdateInfo } from "../tauri-bridge";

/**
 * Checks for an app update on mount and, if one is available, shows a toast
 * offering to install it. Install downloads the signed update, shows progress,
 * then relaunches into the new version.
 *
 * The check is best-effort: any failure (offline, endpoint down) is swallowed
 * so it never blocks or disrupts the app.
 */
export default function UpdateToast() {
	const [info, setInfo] = useState<UpdateInfo | null>(null);
	const [dismissed, setDismissed] = useState(false);
	const [installing, setInstalling] = useState(false);
	const [progress, setProgress] = useState(0);
	const [error, setError] = useState<string | null>(null);

	useEffect(() => {
		let cancelled = false;
		// Check shortly after launch so we don't compete with capture startup.
		const t = setTimeout(async () => {
			try {
				const result = await bridge.checkForUpdates();
				if (!cancelled && result.available) setInfo(result);
			} catch {
				/* offline or endpoint unavailable — ignore */
			}
		}, 5000);
		return () => {
			cancelled = true;
			clearTimeout(t);
		};
	}, []);

	useEffect(() => {
		const unlisten = bridge.onUpdateProgress((pct) => setProgress(pct));
		return () => {
			unlisten.then((u) => u()).catch(() => {});
		};
	}, []);

	if (!info || !info.available || dismissed) return null;

	const handleInstall = async () => {
		setInstalling(true);
		setError(null);
		try {
			// This downloads, installs, and relaunches — the call does not return on success.
			await bridge.installUpdate();
		} catch (e: any) {
			setError(e?.message ?? "Update failed");
			setInstalling(false);
		}
	};

	return (
		<div className="fixed bottom-10 right-6 z-50 slide-in">
			<div className="bg-card border border-[#2e2e00] rounded-xl px-4 py-3 flex items-start gap-3 shadow-2xl max-w-sm">
				<Download size={20} className="text-y flex-shrink-0 mt-0.5" />
				<div className="flex-1 min-w-0">
					<p className="text-white text-sm font-semibold">
						Update available — v{info.version}
					</p>
					<p className="text-text-mid text-xs">
						You're on v{info.current_version}.
					</p>
					{installing ? (
						<div className="mt-2">
							<div className="flex items-center gap-2 text-xs text-text-mid">
								<RefreshCw size={12} className="animate-spin" />
								Downloading… {progress}%
							</div>
							<div className="mt-1 h-1 w-full rounded bg-[#2e2e00] overflow-hidden">
								<div className="h-full bg-y transition-all" style={{ width: `${progress}%` }} />
							</div>
						</div>
					) : (
						<div className="mt-2 flex gap-2">
							<button
								onClick={handleInstall}
								className="bg-y text-black text-xs font-semibold rounded px-3 py-1 hover:opacity-90"
							>
								Install &amp; Restart
							</button>
							<button
								onClick={() => setDismissed(true)}
								className="text-text-dim hover:text-white text-xs px-2 py-1"
							>
								Later
							</button>
						</div>
					)}
					{error && <p className="text-red-400 text-xs mt-1">{error}</p>}
				</div>
				{!installing && (
					<button onClick={() => setDismissed(true)} className="text-text-dim hover:text-white">
						<X size={14} />
					</button>
				)}
			</div>
		</div>
	);
}
