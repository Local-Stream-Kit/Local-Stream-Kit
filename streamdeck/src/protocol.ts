/**
 * Local Stream Kit's control API, as JSON (the Rust side is
 * `crates/protocol/src/control.rs`; the reference is `docs/control-api.md`).
 * Keep the two in step. Fields the plugin doesn't know are ignored, so the
 * app can add some without breaking it.
 */

/** The control API version this plugin speaks (2: `effects` became `plugins`). */
export const CONTROL_VERSION = 2;

export type ControlState = {
	version: number;
	/** In the Cards tab's order. */
	cards: CardState[];
	/** Every plugin's switch, and the text styles': never a fixed list. */
	plugins: PluginState[];
	/** The names Custom event nodes listen for. */
	events: string[];
	queue: QueueSummary;
	/** What a queue's clear can be limited to (`?kind=`). */
	kinds: KindInfo[];
	/** The master volume (0–1) and mute. */
	sound: { volume: number; muted: boolean };
	/** The history's events above its seen marker (apps before 2026-10-14 leave it out). */
	history?: HistorySummary;
};

export type HistorySummary = { new: number };

export type KindInfo = { id: string; name: string };

export type CardState = { id: string; name: string; on: boolean };

export type PluginState = { id: string; name: string; on: boolean };

export type QueueSummary = {
	paused: boolean;
	playing: boolean;
	waiting: number;
	/** What's waiting by kind (`{ follow: 37 }`); kinds with none left out. */
	by_kind: Record<string, number>;
	players: number;
};

export type Switch = "on" | "off" | "toggle";

export type QueueAction = "pause" | "resume" | "toggle" | "skip" | "clear";

/** What the WebSocket sends. The plugin only listens for the state. */
export type ControlMessage =
	| { type: "state"; state: ControlState }
	| { type: "hello"; version: string }
	| { type: "card"; card: CardState }
	| { type: "plugin"; plugin: PluginState }
	| { type: "queue"; queue: QueueSummary }
	| { type: "sound"; sound: ControlState["sound"] }
	| { type: "history"; history: HistorySummary }
	| { type: "fired"; name: string }
	| { type: "error"; status: number; error: string };

export const paths = {
	ws: "/control/ws",
	state: "/control/state",
	card: (id: string, set: Switch) => `/control/cards/${encodeURIComponent(id)}/${set}`,
	plugin: (id: string, set: Switch) => `/control/plugins/${encodeURIComponent(id)}/${set}`,
	/** With `kind`, a clear takes only that kind. */
	queue: (action: QueueAction, kind?: string) =>
		kind ? `/control/queue/${action}?kind=${encodeURIComponent(kind)}` : `/control/queue/${action}`,
	/** `on` mutes, `off` unmutes. */
	mute: (set: Switch) => `/control/mute/${set}`,
	event: (name: string) => `/control/events/${encodeURIComponent(name)}`,
	/** Every event in the history seen. */
	historySeen: "/control/history/seen",
};

/** How the plugin calls itself in its hello. */
export const CLIENT = "streamdeck";

/**
 * Sent on the WebSocket once it's open: which program this is, its version
 * (the manifest's, `0.3.0.0`) and the control API version it speaks. The
 * app shows it on Status, and offers an update when the one built into it
 * is newer, or when this one speaks an older control API.
 */
export function hello(version: string): string {
	return JSON.stringify({ type: "hello", client: CLIENT, version, api: CONTROL_VERSION });
}

/** The key goes in this header on web requests (`?key=` on the WebSocket). */
export const KEY_HEADER = "X-Control-Key";
