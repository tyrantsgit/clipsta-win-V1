import { useEffect, useState } from "react";
import { AlertTriangle, RefreshCw, XCircle, X } from "lucide-react";

type ToastType = "restarted" | "crashed" | "failed";

interface ToastState {
	type: ToastType;
	message: string;
	visible: boolean;
}

export default function CaptureRecoveryToast() {
	const [toast, setToast] = useState<ToastState | null>(null);

	useEffect(() => {
		let unlisteners: (() => void)[] = [];

		import("@tauri-apps/api/event").then(({ listen }) => {
			// Capture process crashed (before respawn attempt)
			listen<{ was_recording: boolean }>("capture:crashed", (event) => {
				setToast({
					type: "crashed",
					message: event.payload.was_recording
						? "Capture crashed during recording — restarting..."
						: "Capture process crashed — restarting...",
					visible: true,
				});
			}).then((u) => unlisteners.push(u));

			// Capture process restarted successfully
			listen<{ recording_resumed: boolean; error?: string }>("capture:restarted", (event) => {
				const { recording_resumed, error } = event.payload;
				setToast({
					type: "restarted",
					message: recording_resumed
						? "Capture recovered — recording resumed"
						: error
						? `Capture restarted (recording lost: ${error})`
						: "Capture process restarted",
					visible: true,
				});
				// Auto-hide after 5 seconds
				setTimeout(() => setToast((prev) => (prev ? { ...prev, visible: false } : null)), 5000);
			}).then((u) => unlisteners.push(u));

			// Capture process failed permanently (crash loop)
			listen<{ reason: string }>("capture:failed", (event) => {
				setToast({
					type: "failed",
					message: event.payload.reason,
					visible: true,
				});
				// Don't auto-hide — user needs to know
			}).then((u) => unlisteners.push(u));
		});

		return () => {
			unlisteners.forEach((u) => u());
		};
	}, []);

	if (!toast || !toast.visible) return null;

	const config = {
		crashed: {
			icon: <RefreshCw size={18} className="text-yellow-400 animate-spin" />,
			border: "border-yellow-500/50",
			bg: "bg-yellow-900/20",
		},
		restarted: {
			icon: <AlertTriangle size={18} className="text-yellow-400" />,
			border: "border-yellow-500/30",
			bg: "bg-card",
		},
		failed: {
			icon: <XCircle size={18} className="text-red-400" />,
			border: "border-red-500/50",
			bg: "bg-red-900/20",
		},
	}[toast.type];

	return (
		<div className="fixed top-16 right-6 z-50 slide-in">
			<div className={`${config.bg} border ${config.border} rounded-xl px-4 py-3 flex items-center gap-3 shadow-2xl max-w-sm`}>
				{config.icon}
				<div className="flex-1 min-w-0">
					<p className="text-white text-sm font-semibold">
						{toast.type === "failed" ? "Capture Failed" : "Capture Recovery"}
					</p>
					<p className="text-text-mid text-xs">{toast.message}</p>
				</div>
				<button onClick={() => setToast((prev) => (prev ? { ...prev, visible: false } : null))} className="text-text-dim hover:text-white">
					<X size={14} />
				</button>
			</div>
		</div>
	);
}
