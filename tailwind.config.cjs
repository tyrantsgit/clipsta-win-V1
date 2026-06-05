/** @type {import('tailwindcss').Config} */
module.exports = {
	content: ["./index.html", "./src/**/*.{ts,tsx}"],
	theme: {
		extend: {
			colors: {
				y: "#D4F000",       // brand yellow
				yd: "#a8bd00",      // yellow dim
				bg: "#0a0a0a",
				card: "#111111",
				card2: "#161616",
				border: "#222222",
				muted: "#2a2a2a",
				"text-dim": "#666666",
				"text-mid": "#999999",
			},
			fontFamily: {
				sans: ["Inter", "system-ui", "sans-serif"],
			},
		},
	},
	plugins: [],
};
