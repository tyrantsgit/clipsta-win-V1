const { createCanvas } = require("canvas");
const c = createCanvas(500, 100);
const ctx = c.getContext("2d");

const tests = [
	`bold 80px "Brush Script MT", "Segoe Script", cursive`,
	`bold 80px "Segoe Script", cursive`,
	`bold 80px cursive`,
	`bold 80px sans-serif`,
];

for (const font of tests) {
	ctx.font = font;
	const m = ctx.measureText("Clipsta");
	console.log(`${font}\n  width=${m.width.toFixed(1)}px\n`);
}
