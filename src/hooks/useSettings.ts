import { useCallback, useEffect, useState } from "react";
import type { AppSettings } from "../types";

export const DEFAULTS: AppSettings = {
	outputFolder: "",
	hotkeyClip1Min: "Alt+F9",
	hotkeyClip5Min: "Alt+F10",
	hotkeyRecord: "F9",
	bufferDuration: 60,
	resolution: "1080p",
	fps: 60,
	aspectRatio: "16:9",
	encoder: "auto",
	bitrate: 8000,
	audioBitrate: 128,
	captureAudio: true,
	captureMic: false,
	audioSource: "desktop",
	audioInputDeviceId: "",
	gameDetect: true,
	autoUpload: false,
	minimizeToTray: true,
	overlayEnabled: true,
	cloudEnabled: false,
	cloudPairCode: "",
	uploadBandwidth: 0,
	deleteAfterUpload: false,
	desktopDeviceId: "",
};

export function useSettings() {
	const [settings, setSettings] = useState<AppSettings>(DEFAULTS);
	const [loaded, setLoaded] = useState(false);

	useEffect(() => {
		(async () => {
			if (window.clipsta) {
				const s = await window.clipsta.getSettings();
				setSettings((prev) => ({ ...prev, ...s }));
			}
			setLoaded(true);
		})();
	}, []);

	const updateSetting = useCallback(<K extends keyof AppSettings>(key: K, value: AppSettings[K]) => {
		setSettings((prev) => ({ ...prev, [key]: value }));
		window.clipsta?.setSetting(key, value);
	}, []);

	const saveAll = useCallback(async (s: AppSettings) => {
		setSettings(s);
		await window.clipsta?.setAllSettings(s);
	}, []);

	return { settings, loaded, updateSetting, saveAll };
}
