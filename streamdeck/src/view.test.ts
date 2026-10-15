import assert from "node:assert/strict";
import { test } from "node:test";

import { parseAddress, webSocketUrl } from "./address.js";
import { keySvg } from "./keys.js";
import type { ControlState } from "./protocol.js";
import { cardView, effectView, eventView, historyView, muteView, queueView } from "./view.js";

const state: ControlState = {
	version: 1,
	cards: [
		{ id: "hydrate", name: "Hydrate", on: true },
		{ id: "party", name: "Party", on: false },
	],
	effects: [{ id: "rain", name: "Emote rain", on: true }],
	events: ["party"],
	queue: { paused: false, playing: true, waiting: 2, by_kind: { follow: 2 }, players: 1 },
	kinds: [
		{ id: "follow", name: "Follows" },
		{ id: "raid", name: "Raids" },
	],
	sound: { volume: 0.8, muted: false },
	history: { new: 4 },
};

test("the address pasted from the panel", () => {
	assert.deepEqual(parseAddress("http://localhost:3000/control/?key=abc"), { base: "http://localhost:3000", key: "abc" });
	assert.deepEqual(parseAddress("  localhost:3100/control/?key=k  "), { base: "http://localhost:3100", key: "k" });
	assert.ok("problem" in parseAddress(""));
	assert.ok("problem" in parseAddress("http://localhost:3000/control/"), "no key");
	assert.ok("problem" in parseAddress("http://localhost:3000/control/?key=…"), "the hidden key");
	assert.ok("problem" in parseAddress("ftp://localhost/?key=a"));
	const address = { base: "http://localhost:3000", key: "a b" };
	assert.equal(webSocketUrl(address, "/control/ws"), "ws://localhost:3000/control/ws?key=a+b");
});

test("a card key shows whether the card is on, by id", () => {
	assert.deepEqual(cardView({ card: "hydrate", name: "Old name" }, state, "connected"), { look: "on", label: "ON", title: "Hydrate" });
	assert.deepEqual(cardView({ card: "party", set: "toggle" }, state, "connected"), { look: "off", label: "OFF", title: "Party" });
	assert.equal(cardView({ card: "hydrate", set: "on" }, state, "connected").label, "IS ON");
	assert.deepEqual(cardView({ card: "gone", name: "Gone" }, state, "connected"), { look: "missing", label: "DELETED", title: "Gone" });
	assert.deepEqual(cardView({}, state, "connected").title, "Pick a card");
	// While the app is away, the name it had.
	assert.deepEqual(cardView({ card: "hydrate", name: "Hydrate" }, undefined, "unreachable"), {
		look: "offline",
		label: "OFFLINE",
		title: "Hydrate",
	});
	assert.equal(cardView({ card: "hydrate" }, undefined, "wrong-key").label, "WRONG KEY");
});

test("effect, queue and event keys", () => {
	assert.deepEqual(effectView({ effect: "rain" }, state, "connected"), { look: "on", label: "ON", title: "Emote rain" });
	assert.equal(effectView({ effect: "confetti", name: "Confetti" }, state, "connected").look, "missing");
	assert.deepEqual(queueView({}, state, "connected"), { look: "on", label: "PLAYING", title: "Queue" });
	const paused = { ...state, queue: { ...state.queue, paused: true } };
	assert.deepEqual(queueView({ action: "pause" }, paused, "connected"), { look: "paused", label: "PAUSED", title: "Pause" });
	assert.deepEqual(queueView({ action: "clear" }, state, "connected"), { look: "plain", label: "2 WAITING", title: "Clear" });
	assert.equal(queueView({ action: "skip" }, { ...state, queue: { ...state.queue, playing: false } }, "connected").label, "IDLE");
	assert.deepEqual(queueView({ action: "clear", kind: "follow" }, state, "connected"), {
		look: "plain",
		label: "2 WAITING",
		title: "Clear follows",
	});
	assert.equal(queueView({ action: "clear", kind: "raid" }, state, "connected").label, "0 WAITING");
	assert.equal(queueView({ action: "clear", kind: "raid", name: "Raids" }, undefined, "unreachable").title, "Clear raids");
	assert.deepEqual(muteView({}, state, "connected"), { look: "on", label: "80%", title: "Sound" });
	const muted = { ...state, sound: { volume: 0.8, muted: true } };
	assert.deepEqual(muteView({ set: "on" }, muted, "connected"), { look: "paused", label: "MUTED", title: "Mute" });
	assert.equal(muteView({ set: "off" }, undefined, "unreachable").title, "Unmute");
	assert.deepEqual(eventView({ event: "party" }, "connected"), { look: "plain", label: "FIRE", title: "party" });

	assert.equal(eventView({ event: "party" }, "connecting").look, "offline");
});

test("key images escape what they show", () => {
	const svg = keySvg("card", { look: "on", label: "<A&B>", title: "" });
	assert.ok(svg.includes("&lt;A&amp;B&gt;"));
	assert.ok(svg.startsWith("<svg"));
});

test("the history key shows how many events are new", () => {
	assert.deepEqual(historyView(state, "connected"), { look: "plain", label: "4 NEW", title: "History" });
	assert.deepEqual(historyView({ ...state, history: { new: 0 } }, "connected"), { look: "off", label: "ALL SEEN", title: "History" });
	const { history: _, ...older } = state;
	assert.equal(historyView(older, "connected").label, "ALL SEEN", "an app from before the marker");
	assert.equal(historyView(state, "unreachable").label, "OFFLINE");
});
