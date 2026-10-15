// Draws the plugin's images into the .sdPlugin folder: `npm run icons`.
// The action list icons are white on transparent (Stream Deck tints them),
// the keys' default images are the ones the plugin draws (`src/keys.ts`),
// and the plugin icon has to be a PNG, so it's rasterised here.
import { mkdirSync, writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

import { ICONS, keySvg } from "../src/keys.ts";

const out = new URL("../com.localstreamkit.streamdeck.sdPlugin/imgs/", import.meta.url);
mkdirSync(new URL("actions/", out), { recursive: true });
mkdirSync(new URL("keys/", out), { recursive: true });


const mono = (path, size) =>
	`<svg xmlns="http://www.w3.org/2000/svg" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="#ffffff" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="${path}"/></svg>\n`;

for (const kind of ["card", "effect", "queue", "event", "mute", "history"]) {
	const path = ICONS[kind];
	writeFileSync(new URL(`actions/${kind}.svg`, out), mono(path, 20));
	writeFileSync(new URL(`keys/${kind}.svg`, out), keySvg(kind, { look: "plain", label: "", title: "" }) + "\n");
}
writeFileSync(new URL("category.svg", out), mono(ICONS.event, 28));

// The plugin icon: a lightning bolt on the panel's purple, as a PNG.
const BOLT = [
	[13, 2],
	[5, 14],
	[11, 14],
	[10, 22],
	[18, 10],
	[12, 10],
];

function inside(x, y, points) {
	let hit = false;
	for (let i = 0, j = points.length - 1; i < points.length; j = i++) {
		const [xi, yi] = points[i];
		const [xj, yj] = points[j];
		if (yi > y !== yj > y && x < ((xj - xi) * (y - yi)) / (yj - yi) + xi) hit = !hit;
	}
	return hit;
}

function roundedSquare(x, y, size, radius) {
	const cx = Math.min(Math.max(x, radius), size - radius);
	const cy = Math.min(Math.max(y, radius), size - radius);
	return (x - cx) ** 2 + (y - cy) ** 2 <= radius ** 2;
}

function png(size) {
	const samples = 4;
	const rows = [];
	const bolt = BOLT.map(([x, y]) => [((x - 12) * 0.62 + 12) * (size / 24), ((y - 12) * 0.62 + 12) * (size / 24)]);
	for (let y = 0; y < size; y++) {
		const row = Buffer.alloc(1 + size * 4);
		for (let x = 0; x < size; x++) {
			let square = 0;
			let white = 0;
			for (let sy = 0; sy < samples; sy++) {
				for (let sx = 0; sx < samples; sx++) {
					const px = x + (sx + 0.5) / samples;
					const py = y + (sy + 0.5) / samples;
					if (roundedSquare(px, py, size, size * 0.22)) {
						square++;
						if (inside(px, py, bolt)) white++;
					}
				}
			}
			const total = samples * samples;
			const w = white / Math.max(square, 1);
			row[1 + x * 4] = Math.round(0x6c + (0xff - 0x6c) * w);
			row[2 + x * 4] = Math.round(0x5c + (0xff - 0x5c) * w);
			row[3 + x * 4] = Math.round(0xe7 + (0xff - 0xe7) * w);
			row[4 + x * 4] = Math.round((255 * square) / total);
		}
		rows.push(row);
	}
	const crcTable = Array.from({ length: 256 }, (_, n) => {
		let c = n;
		for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
		return c >>> 0;
	});
	const crc = (buffer) => {
		let c = 0xffffffff;
		for (const byte of buffer) c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
		return (c ^ 0xffffffff) >>> 0;
	};
	const chunk = (type, data) => {
		const length = Buffer.alloc(4);
		length.writeUInt32BE(data.length);
		const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
		const check = Buffer.alloc(4);
		check.writeUInt32BE(crc(body));
		return Buffer.concat([length, body, check]);
	};
	const header = Buffer.alloc(13);
	header.writeUInt32BE(size, 0);
	header.writeUInt32BE(size, 4);
	header[8] = 8; // bits per channel
	header[9] = 6; // RGBA
	return Buffer.concat([
		Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
		chunk("IHDR", header),
		chunk("IDAT", deflateSync(Buffer.concat(rows))),
		chunk("IEND", Buffer.alloc(0)),
	]);
}

writeFileSync(new URL("plugin.png", out), png(256));
writeFileSync(new URL("plugin@2x.png", out), png(512));
console.log("drew the images in", out.pathname);
