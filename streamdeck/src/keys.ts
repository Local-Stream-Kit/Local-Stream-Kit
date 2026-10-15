/**
 * The key images, drawn as SVG: the colour says on / off / paused at a
 * glance, an icon says what kind of key it is, and a word says the state.
 * Stream Deck draws the title (the card's name) under it.
 */
import type { QueueAction } from "./protocol.js";
import type { Kind, KeyView, Look } from "./view.js";

/** A key's icon: its kind's, or a queue key's or mute key's own. */
export type Icon = Kind | QueueAction | "unmute";

const BACKGROUND: Record<Look, string> = {
	on: "#1f7a4a",
	off: "#2a2c33",
	paused: "#8a5a12",
	plain: "#3b3270",
	missing: "#6b1f24",
	offline: "#202125",
};

const LABEL: Record<Look, string> = {
	on: "#c9f5dc",
	off: "#9a9ca6",
	paused: "#ffe2a8",
	plain: "#ddd8ff",
	missing: "#ffc4c4",
	offline: "#8a8c94",
};

/** Icons on a 24 × 24 grid, stroked in white. */
export const ICONS: Record<Icon, string> = {
	card: "M5 4h14v16H5zM8 9h8M8 13h8M8 17h5",
	effect: "M12 3l1.9 5.6L19.5 10l-5.6 1.9L12 17.5l-1.9-5.6L4.5 10l5.6-1.4zM18.5 16l.8 2.2 2.2.8-2.2.8-.8 2.2-.8-2.2-2.2-.8 2.2-.8z",
	queue: "M4 6h16M4 12h16M4 18h10",
	event: "M13 2 5 14h6l-1 8 8-12h-6z",
	toggle: "M8 5v14M16 5v14",
	pause: "M8 5v14M16 5v14",
	resume: "M7 4v16l13-8z",
	skip: "M5 5v14l10-7zM19 5v14",
	clear: "M4 7h16M9 7V4h6v3M6 7l1 13h10l1-13",
	mute: "M4 9h4l5-4v14l-5-4H4zM16.5 9.5l5 5M21.5 9.5l-5 5",
	unmute: "M4 9h4l5-4v14l-5-4H4zM16.5 8.5a5 5 0 0 1 0 7M19 6a8.5 8.5 0 0 1 0 12",
	history: "M4.5 12.5l4.5 4.5L19.5 6.5",
};

function escapeXml(text: string): string {
	return text.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}

/** The SVG for a key (144 × 144). `icon` picks the queue key's icon. */
export function keySvg(kind: Kind, view: KeyView, icon: Icon = kind): string {
	const label = view.label
		? `<text x="72" y="98" text-anchor="middle" font-family="Arial, Helvetica, sans-serif" font-weight="700" font-size="${view.label.length > 8 ? 17 : 21}" letter-spacing="1" fill="${LABEL[view.look]}">${escapeXml(view.label)}</text>`
		: "";
	const iconY = view.label ? 16 : 34;
	return (
		`<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">` +
		`<rect width="144" height="144" fill="${BACKGROUND[view.look]}"/>` +
		`<g transform="translate(46 ${iconY}) scale(2.1667)" fill="none" stroke="#ffffff" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" opacity="${view.look === "offline" ? 0.45 : 0.95}">` +
		`<path d="${ICONS[icon]}"/></g>` +
		label +
		`</svg>`
	);
}

/** The key image as Stream Deck takes it. */
export function keyImage(kind: Kind, view: KeyView, icon: Icon = kind): string {
	return `data:image/svg+xml;charset=utf8,${encodeURIComponent(keySvg(kind, view, icon))}`;
}
