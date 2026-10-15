/**
 * What each key shows, from the app's state and the key's settings. No
 * Stream Deck calls here, so it can be tested on its own.
 */
import type { ControlState, QueueAction, Switch } from "./protocol.js";

/** How the plugin's connection to Local Stream Kit is doing. */
export type Connection =
	/** No address pasted yet. */
	| "unset"
	| "connecting"
	| "connected"
	/** The app answered 401. */
	| "wrong-key"
	/** Nothing answers at the address. */
	| "unreachable"
	/** The app speaks a newer control API than this plugin. */
	| "too-new";

export type Look = "on" | "off" | "paused" | "plain" | "missing" | "offline";

export type KeyView = {
	look: Look;
	/** Drawn on the key's image, under the icon. */
	label: string;
	/** Stream Deck's title, under the image. */
	title: string;
};

export type Kind = "card" | "effect" | "queue" | "event" | "mute" | "history";

/** Each kind of key's settings, saved by Stream Deck per key. */
export type CardSettings = {
	/** The card's id: it stays when the card is renamed. */
	card?: string;
	/** Its name when picked, for while the app is away. */
	name?: string;
	set?: Switch;
};
export type EffectSettings = { effect?: string; name?: string; set?: Switch };
export type QueueSettings = {
	action?: QueueAction;
	/** A clear takes only this kind (`follow`). */
	kind?: string;
	/** Its name when picked ("Follows"), for while the app is away. */
	name?: string;
};
export type EventSettings = { event?: string };
/** `on` mutes, `off` unmutes. */
export type MuteSettings = { set?: Switch };
/** Mark history seen has nothing to set. */
export type HistorySettings = Record<string, never>;

export type Settings = CardSettings | EffectSettings | QueueSettings | EventSettings | MuteSettings | HistorySettings;

export const SWITCHES: { value: Switch; label: string }[] = [
	{ value: "toggle", label: "Toggle" },
	{ value: "on", label: "Turn on" },
	{ value: "off", label: "Turn off" },
];

export const QUEUE_ACTIONS: { value: QueueAction; label: string; title: string }[] = [
	{ value: "toggle", label: "Pause / resume", title: "Queue" },
	{ value: "pause", label: "Pause", title: "Pause" },
	{ value: "resume", label: "Resume", title: "Resume" },
	{ value: "skip", label: "Skip what's playing", title: "Skip" },
	{ value: "clear", label: "Clear what's waiting", title: "Clear" },
];

export const MUTE_SWITCHES: { value: Switch; label: string; title: string }[] = [
	{ value: "toggle", label: "Mute / unmute", title: "Sound" },
	{ value: "on", label: "Mute", title: "Mute" },
	{ value: "off", label: "Unmute", title: "Unmute" },
];

/** A short word for a key while the app can't be reached. */
export function offlineLabel(connection: Connection): string {
	switch (connection) {
		case "unset":
			return "SET UP";
		case "wrong-key":
			return "WRONG KEY";
		case "too-new":
			return "UPDATE";
		default:
			return "OFFLINE";
	}
}

export function keyView(kind: Kind, settings: Settings, state: ControlState | undefined, connection: Connection): KeyView {
	switch (kind) {
		case "card":
			return cardView(settings as CardSettings, state, connection);
		case "effect":
			return effectView(settings as EffectSettings, state, connection);
		case "queue":
			return queueView(settings as QueueSettings, state, connection);
		case "event":
			return eventView(settings as EventSettings, connection);
		case "mute":
			return muteView(settings as MuteSettings, state, connection);
		case "history":
			return historyView(state, connection);
	}
}

/** How many events are new in the history: above its seen marker. */
export function historyView(state: ControlState | undefined, connection: Connection): KeyView {
	const title = "History";
	if (connection !== "connected" || !state) {
		return offline(title, connection);
	}
	const fresh = state.history?.new ?? 0;
	return fresh > 0 ? { look: "plain", label: `${fresh} NEW`, title } : { look: "off", label: "ALL SEEN", title };
}

/** MUTED, or the master volume. */
export function muteView(settings: MuteSettings, state: ControlState | undefined, connection: Connection): KeyView {
	const set = MUTE_SWITCHES.find((known) => known.value === (settings.set ?? "toggle")) ?? MUTE_SWITCHES[0];
	if (connection !== "connected" || !state) {
		return offline(set.title, connection);
	}
	const { volume, muted } = state.sound;
	return { look: muted ? "paused" : "on", label: muted ? "MUTED" : `${Math.round(volume * 100)}%`, title: set.title };
}

function offline(title: string, connection: Connection): KeyView {
	return { look: "offline", label: offlineLabel(connection), title };
}

function switched(on: boolean, set: Switch | undefined): string {
	// A key that only turns on (or off) says what it does when it's done.
	if (set === "on" && on) return "IS ON";
	if (set === "off" && !on) return "IS OFF";
	return on ? "ON" : "OFF";
}

export function cardView(settings: CardSettings, state: ControlState | undefined, connection: Connection): KeyView {
	if (!settings.card) {
		return { look: "plain", label: "", title: "Pick a card" };
	}
	const known = settings.name ?? settings.card;
	if (connection !== "connected" || !state) {
		return offline(known, connection);
	}
	const card = state.cards.find((card) => card.id === settings.card);
	if (!card) {
		return { look: "missing", label: "DELETED", title: known };
	}
	return { look: card.on ? "on" : "off", label: switched(card.on, settings.set), title: card.name };
}

export function effectView(settings: EffectSettings, state: ControlState | undefined, connection: Connection): KeyView {
	if (!settings.effect) {
		return { look: "plain", label: "", title: "Pick an effect" };
	}
	const known = settings.name ?? settings.effect;
	if (connection !== "connected" || !state) {
		return offline(known, connection);
	}
	const effect = state.effects.find((effect) => effect.id === settings.effect);
	if (!effect) {
		return { look: "missing", label: "GONE", title: known };
	}
	return { look: effect.on ? "on" : "off", label: switched(effect.on, settings.set), title: effect.name };
}

export function queueView(settings: QueueSettings, state: ControlState | undefined, connection: Connection): KeyView {
	const action = QUEUE_ACTIONS.find((known) => known.value === (settings.action ?? "toggle")) ?? QUEUE_ACTIONS[0];
	const kind = action.value === "clear" ? settings.kind : undefined;
	const kindName = kind ? (state?.kinds.find((known) => known.id === kind)?.name ?? settings.name ?? kind) : undefined;
	const title = kindName ? `Clear ${kindName.toLowerCase()}` : action.title;
	if (connection !== "connected" || !state) {
		return offline(title, connection);
	}
	const queue = state.queue;
	switch (action.value) {
		case "toggle":
		case "pause":
		case "resume":
			return { look: queue.paused ? "paused" : "on", label: queue.paused ? "PAUSED" : "PLAYING", title: action.title };
		case "skip":
			return { look: queue.playing ? "plain" : "off", label: queue.playing ? "SKIP" : "IDLE", title: action.title };
		case "clear": {
			const waiting = kind ? (queue.by_kind[kind] ?? 0) : queue.waiting;
			return { look: waiting > 0 ? "plain" : "off", label: `${waiting} WAITING`, title };
		}
	}
}

export function eventView(settings: EventSettings, connection: Connection): KeyView {
	if (!settings.event) {
		return { look: "plain", label: "", title: "Pick an event" };
	}
	if (connection !== "connected") {
		return offline(settings.event, connection);
	}
	return { look: "plain", label: "FIRE", title: settings.event };
}
