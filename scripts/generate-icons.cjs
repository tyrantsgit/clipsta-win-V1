const { createCanvas } = require("canvas");
const fs = require("fs");
const path = require("path");
const pngToIco = require("png-to-ico").default;

const NEON = "#B8FF00";
const BRIGHT = "#E6FF66";
const BG = "#0B0B0B";
const CARD = "#141414";
const SIZES = [16, 24, 32, 48, 64, 128, 256];

function drawSmallC(ctx, size) {
	// For 16-32px: bold filled C using thick letter
	const s = size;
	const cx = s / 2;
	const cy = s / 2;
	const fontSize = Math.max(9, Math.round(s * 0.7));
	ctx.font = `bold ${fontSize}px "Segoe UI", "Arial", sans-serif`;
	ctx.textAlign = "center";
	ctx.textBaseline = "middle";
	ctx.shadowColor = NEON;
	ctx.shadowBlur = Math.max(1, s * 0.08);
	ctx.fillStyle = BRIGHT;
	ctx.fillText("C", cx, cy + 0.5);
}

function drawLargeC(ctx, w, h) {
	const s = Math.min(w, h);
	const cx = w / 2;
	const cy = h / 2;

	ctx.save();
	ctx.translate(cx, cy);
	ctx.scale(s / 256, s / 256);

	// Subtle card background circle
	ctx.beginPath();
	ctx.arc(0, 0, 105, 0, Math.PI * 2);
	ctx.fillStyle = CARD;
	ctx.fill();

	// Outer glow ring
	ctx.beginPath();
	ctx.arc(0, 0, 92, 0, Math.PI * 2);
	ctx.strokeStyle = "rgba(184,255,0,0.15)";
	ctx.lineWidth = 2;
	ctx.stroke();

	// Filled "C" shape using clipping
	const outerR = 80;
	const innerR = 42;

	// Draw a filled C by creating the outer arc + inner arc + connecting lines
	// The "C" opens on the right side (between angles -0.5 and 0.5 radians)

	const startA = 0.55;
	const endA = Math.PI * 2 - 0.55;

	ctx.beginPath();
	// Outer arc: from startA to endA (clockwise)
	ctx.arc(0, 0, outerR, startA, endA);
	// Line to inner arc end
	ctx.lineTo(Math.cos(endA) * innerR, Math.sin(endA) * innerR);
	// Inner arc: from endA to startA (counter-clockwise)
	ctx.arc(0, 0, innerR, endA, startA, true);
	ctx.closePath();

	// Glow
	ctx.shadowColor = NEON;
	ctx.shadowBlur = 24;

	const grad = ctx.createLinearGradient(-70, -70, 70, 70);
	grad.addColorStop(0, BRIGHT);
	grad.addColorStop(1, NEON);
	ctx.fillStyle = grad;
	ctx.fill();

	// Inner edge highlight
	ctx.shadowBlur = 0;
	ctx.beginPath();
	ctx.arc(0, 0, innerR + 2, startA + 0.1, endA - 0.1);
	ctx.strokeStyle = "rgba(230,255,102,0.2)";
	ctx.lineWidth = 2;
	ctx.stroke();

	// Center accent dot
	ctx.beginPath();
	ctx.arc(0, 0, 3.5, 0, Math.PI * 2);
	ctx.fillStyle = BRIGHT;
	ctx.fill();

	ctx.restore();
}

function drawCLogo(ctx, w, h) {
	const size = Math.min(w, h);
	if (size <= 32) {
		drawSmallC(ctx, size);
	} else {
		drawLargeC(ctx, w, h);
	}
}

async function main() {
	const outDir = path.join(__dirname, "..", "build");
	if (!fs.existsSync(outDir)) fs.mkdirSync(outDir, { recursive: true });

	const pngBuffers = [];
	for (const size of SIZES) {
		const canvas = createCanvas(size, size);
		const ctx = canvas.getContext("2d");
		// Always fill background first (handled inside draw functions for large, but do here for all)
		ctx.fillStyle = BG;
		ctx.fillRect(0, 0, size, size);
		drawCLogo(ctx, size, size);
		const buf = canvas.toBuffer("image/png");
		fs.writeFileSync(path.join(outDir, `icon-${size}.png`), buf);
		console.log(`  build/icon-${size}.png`);
		pngBuffers.push(buf);
	}

	const icoBuf = await pngToIco(pngBuffers);
	fs.writeFileSync(path.join(outDir, "icon.ico"), icoBuf);
	console.log("  build/icon.ico");

	fs.copyFileSync(path.join(outDir, "icon.ico"), path.join(__dirname, "..", "public", "icon.ico"));
	fs.copyFileSync(path.join(outDir, "icon-16.png"), path.join(__dirname, "..", "public", "tray.png"));
	fs.copyFileSync(path.join(outDir, "icon-256.png"), path.join(__dirname, "..", "public", "icon.png"));
	console.log("  -> public/icon.ico");
	console.log("  -> public/tray.png");
	console.log("  -> public/icon.png");

	console.log("\nDone!");
}

main().catch(console.error);
