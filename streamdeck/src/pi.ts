/**
 * A key's settings page ("property inspector"), shown by Stream Deck
 * beside the key: the app's address (shared by every key), whether the
 * plugin is connected, and dropdowns filled from the app, so nothing is
 * typed. Stream Deck calls `connectElgatoStreamDeckSocket` once it's loaded.
 */
import type { FromSettingsPage, ToSettingsPage } from "./pi-messages.js";
import {
	type CardSettings,
	type EffectSettings,
	type EventSettings,
	type Kind,
	MUTE_SWITCHES,
	type MuteSettings,
	QUEUE_ACTIONS,
	type QueueSettings,
	type Settings,
	SWITCHES,
} from "./view.js";

type ActionInfo = { action: string; context: string; payload: { settings: Settings } };

let socket: WebSocket;
let uuid = "";
let actionUuid = "";
let kind: Kind = "card";
let settings: Settings = {};
let view: ToSettingsPage | undefined;

const element = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

function send(message: object): void {
	socket.send(JSON.stringify(message));
}

function toPlugin(payload: FromSettingsPage): void {
	send({ event: "sendToPlugin", action: actionUuid, context: uuid, payload });
}

function save(changed: Settings): void {
	settings = { ...settings, ...changed };
	send({ event: "setSettings", context: uuid, payload: settings });
	render();
}

(window as unknown as Record<string, unknown>).connectElgatoStreamDeckSocket = (
	port: string,
	propertyInspectorUuid: string,
	registerEvent: string,
	_info: string,
	actionInfo: string,
) => {
	uuid = propertyInspectorUuid;
	const info = JSON.parse(actionInfo) as ActionInfo;
	actionUuid = info.action;
	kind = (info.action.split(".").pop() ?? "card") as Kind;
	settings = info.payload.settings ?? {};
	socket = new WebSocket(`ws://127.0.0.1:${port}`);
	socket.addEventListener("open", () => {
		send({ event: registerEvent, uuid });
		toPlugin({ type: "hello" });
	});
	socket.addEventListener("message", (event) => {
		const message = JSON.parse(event.data);
		if (message.event === "sendToPropertyInspector") {
			view = message.payload as ToSettingsPage;
			render();
		} else if (message.event === "didReceiveSettings") {
			settings = message.payload.settings ?? {};
			render();
		}
	});
	element<HTMLInputElement>("address").addEventListener("change", (event) => {
		const text = (event.target as HTMLInputElement).value;
		if (text.trim()) {
			toPlugin({ type: "address", text });
			(event.target as HTMLInputElement).value = "";
		}
	});
	render();
};

const STATUS: Record<ToSettingsPage["connection"], string> = {
	unset: "Not set up",
	connecting: "Connecting…",
	connected: "Connected",
	"wrong-key": "Wrong key",
	unreachable: "Not running?",
	"too-new": "Update the plugin",
};

function render(): void {
	const connection = view?.connection ?? "connecting";
	const status = element("status");
	status.textContent = connection === "connected" && view?.base ? `Connected to ${view.base}` : STATUS[connection];
	status.className = `value status ${connection}`;
	const problem = element("problem");
	problem.textContent = view?.problem ?? "";
	problem.hidden = !view?.problem;
	element<HTMLInputElement>("address").placeholder =
		connection === "unset" ? "http://localhost:5750/control/?key=…" : "Paste a new address to change it";
	element("address-hint").hidden = connection === "connected";
	renderFields();
}

type Option = { value: string; label: string };

/** A labelled dropdown. A `current` value that isn't offered stays, marked. */
function dropdown(label: string, options: Option[], current: string | undefined, missing: string, pick: (value: string) => void): HTMLElement {
	const row = document.createElement("div");
	row.className = "row";
	const name = document.createElement("label");
	name.className = "label";
	name.textContent = label;
	const select = document.createElement("select");
	select.className = "value";
	const all = [...options];
	if (current && !options.some((option) => option.value === current)) {
		all.unshift({ value: current, label: missing });
	}
	if (!current) {
		all.unshift({ value: "", label: options.length ? "Pick one" : "Nothing to pick yet" });
	}
	for (const option of all) {
		const element = document.createElement("option");
		element.value = option.value;
		element.textContent = option.label;
		element.selected = option.value === (current ?? "");
		element.disabled = option.value === "";
		select.append(element);
	}
	select.addEventListener("change", () => pick(select.value));
	name.htmlFor = select.id = `field-${label.toLowerCase().replace(/\W+/g, "-")}`;
	row.append(name, select);
	return row;
}

function hint(text: string): HTMLElement {
	const row = document.createElement("div");
	row.className = "row";
	const spacer = document.createElement("span");
	spacer.className = "label";
	const note = document.createElement("span");
	note.className = "value hint";
	note.textContent = text;
	row.append(spacer, note);
	return row;
}

function renderFields(): void {
	const fields = element("fields");
	const focused = document.activeElement?.id;
	fields.replaceChildren();
	const offline = view?.connection !== "connected";
	const switches = SWITCHES.map(({ value, label }) => ({ value, label }));
	switch (kind) {
		case "card": {
			const card = settings as CardSettings;
			const cards = (view?.cards ?? []).map((c) => ({ value: c.id, label: c.on ? c.name : `${c.name} (off)` }));
			const missing = offline ? (card.name ?? card.card ?? "") : `${card.name ?? card.card} (deleted)`;
			fields.append(
				dropdown("Card", cards, card.card, missing, (id) => {
					save({ card: id, name: view?.cards.find((c) => c.id === id)?.name } as CardSettings);
				}),
				dropdown("When pressed", switches, card.set ?? "toggle", "", (set) => save({ set } as CardSettings)),
			);
			break;
		}
		case "effect": {
			const effect = settings as EffectSettings;
			const effects = (view?.effects ?? []).map((e) => ({ value: e.id, label: e.on ? e.name : `${e.name} (off)` }));
			const missing = offline ? (effect.name ?? effect.effect ?? "") : `${effect.name ?? effect.effect} (gone)`;
			fields.append(
				dropdown("Effect", effects, effect.effect, missing, (id) => {
					save({ effect: id, name: view?.effects.find((e) => e.id === id)?.name } as EffectSettings);
				}),
				dropdown("When pressed", switches, effect.set ?? "toggle", "", (set) => save({ set } as EffectSettings)),
			);
			break;
		}
		case "queue": {
			const queue = settings as QueueSettings;
			const actions = QUEUE_ACTIONS.map(({ value, label }) => ({ value, label }));
			fields.append(dropdown("When pressed", actions, queue.action ?? "toggle", "", (action) => save({ action } as QueueSettings)));
			if ((queue.action ?? "toggle") === "clear") {
				// "all" stands for no kind: an empty value is the "Pick one" line.
				const kinds = [{ value: "all", label: "Everything" }, ...(view?.kinds ?? []).map((k) => ({ value: k.id, label: k.name }))];
				const missing = queue.name ?? queue.kind ?? "";
				fields.append(
					dropdown("Only", kinds, queue.kind ?? "all", missing, (kind) => {
						const picked = view?.kinds.find((k) => k.id === kind);
						save({ kind: picked?.id, name: picked?.name } as QueueSettings);
					}),
				);
			}
			break;
		}
		case "mute": {
			const mute = settings as MuteSettings;
			const sets = MUTE_SWITCHES.map(({ value, label }) => ({ value, label }));
			fields.append(dropdown("When pressed", sets, mute.set ?? "toggle", "", (set) => save({ set } as MuteSettings)));
			fields.append(hint("Everything on the overlays plays on, silently: unmute in the middle of a message, and the rest of it is heard."));
			break;
		}
		case "history":
			fields.append(hint("Pressed: every event in the History counts as seen, as the panel's All seen does. The key shows how many are new."));
			break;
		case "event": {
			const event = settings as EventSettings;
			const events = (view?.events ?? []).map((name) => ({ value: name, label: name }));
			const missing = offline ? (event.event ?? "") : `${event.event} (no card listens)`;
			fields.append(dropdown("Event", events, event.event, missing, (name) => save({ event: name } as EventSettings)));
			if (!offline && events.length === 0) {
				fields.append(hint("No card listens for a Custom event yet: start a card in Local Stream Kit with a Custom event node, and give it a name."));
			}
			break;
		}
	}
	if (focused) {
		document.getElementById(focused)?.focus();
	}
}
