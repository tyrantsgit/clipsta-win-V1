import { useEffect, useState } from "react";
import { CheckCircle, X, FolderOpen } from "lucide-react";
import bridge from "../tauri-bridge";

export default function ExportToast({ path }: { path: string | null }) {
	const [visible, setVisible] = useState(false);
	const [current, setCurrent] = useState<string | null>(null);

	useEffect(() => {
		if (!path) return;
		setCurrent(path);
		setVisible(true);
		const t = setTimeout(() => setVisible(false), 8000);
		return () => clearTimeout(t);
	}, [path]);

	if (!visible || !current) return null;

	const name = current.split(/[\\/]/).pop() ?? current;

	return (
		<div className="fixed bottom-10 left-1/2 -translate-x-1/2 z-50 slide-in">
			<div className="bg-[#0a1a00] border border-[#2a4a00] rounded-xl px-5 py-3 flex items-center gap-3 shadow-2xl">
				<CheckCircle size={20} className="text-y flex-shrink-0" />
				<div className="flex-1 min-w-0">
					<p className="text-white text-sm font-semibold">Export complete!</p>
					<p className="text-text-mid text-xs truncate max-w-[200px]">{name}</p>
				</div>
				<button
					onClick={() => bridge.showInFolder(current)}
					className="flex items-center gap-1 px-2.5 py-1.5 rounded bg-y text-black text-xs font-bold hover:bg-yd transition-colors"
				>
					<FolderOpen size={12} /> Open
				</button>
				<button onClick={() => setVisible(false)} className="text-text-dim hover:text-white ml-1">
					<X size={14} />
				</button>
			</div>
		</div>
	);
}
