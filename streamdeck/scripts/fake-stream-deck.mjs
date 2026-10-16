// Runs the built plugin against a stand-in Stream Deck and a running
// Local Stream Kit, without a Stream Deck: `npm run try -- <address>`, with
// the address from the panel's Control API page. It shows a card key, an
// effect key, a queue key and an event key, presses some, opens a key's
// settings page, and checks what the plugin draws and asks for. The card
// and effect it presses are switched back afterwards.
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";

import { WebSocketServer } from "ws";

const address = process.argv[2];
if (!address) {
	console.error("usage: npm run try -- http://localhost:5750/control/?key=…");
	process.exit(2);
}
const url = new URL(address);
const key = url.searchParams.get("key");
const base = url.origin;
const UUID = "com.localstreamkit.streamdeck";
const plugin = fileURLToPath(new URL(`../${UUID}.sdPlugin/bin/plugin.js`, import.meta.url));

const state = async () => (await fetch(`${base}/control/state?key=${key}`)).json();
const before = await state();
const card = before.cards[0];
const event = before.events[0];

const server = new WebSocketServer({ port: 0, host: "127.0.0.1" });
await new Promise((listening) => server.once("listening", listening));
const port = server.address().port;
/** What the plugin sent, newest last. */
const sent = [];
let socket;
server.on("connection", (connection) => {
	socket = connection;
	connection.on("message", (data) => {
		const message = JSON.parse(data.toString());
		sent.push(message);
		if (message.event === "getGlobalSettings") {
			reply({ event: "didReceiveGlobalSettings", payload: { settings: { address } } });
		} else if (message.event === "getSettings") {
			const key = keys.find((k) => k.context === message.context);
			reply({ event: "didReceiveSettings", action: key.action, context: key.context, device: "deck", payload: { settings: key.settings, coordinates: key.coordinates, controller: "Keypad", isInMultiAction: false } });
		}
	});
});
const reply = (message) => socket.send(JSON.stringify(message));

const keys = [
	{ action: `${UUID}.card`, context: "k-card", settings: { card: card.id, name: card.name, set: "toggle" }, coordinates: { column: 0, row: 0 } },
	{ action: `${UUID}.effect`, context: "k-effect", settings: { effect: "rain", name: "Emote rain" }, coordinates: { column: 1, row: 0 } },
	{ action: `${UUID}.queue`, context: "k-queue", settings: { action: "toggle" }, coordinates: { column: 2, row: 0 } },
	{ action: `${UUID}.event`, context: "k-event", settings: { event }, coordinates: { column: 3, row: 0 } },
	{ action: `${UUID}.card`, context: "k-gone", settings: { card: "no-such-card", name: "Old card" }, coordinates: { column: 4, row: 0 } },
	{ action: `${UUID}.mute`, context: "k-mute", settings: { set: "toggle" }, coordinates: { column: 0, row: 1 } },
	{ action: `${UUID}.queue`, context: "k-clear", settings: { action: "clear", kind: "follow", name: "Follows" }, coordinates: { column: 1, row: 1 } },
	{ action: `${UUID}.history`, context: "k-history", settings: {}, coordinates: { column: 2, row: 1 } },
];
const payload = (k) => ({ settings: k.settings, coordinates: k.coordinates, controller: "Keypad", isInMultiAction: false, state: 0 });

const info = {
	application: { font: "Segoe UI", language: "en", platform: "windows", platformVersion: "10.0.26200", version: "7.5.1.22901" },
	// As Stream Deck does, from the manifest: the plugin says it in its hello.
	plugin: { uuid: UUID, version: JSON.parse(readFileSync(new URL(`../${UUID}.sdPlugin/manifest.json`, import.meta.url), "utf8")).Version },
	devicePixelRatio: 2,
	colors: {},
	devices: [{ id: "deck", name: "Fake Stream Deck", size: { columns: 5, rows: 3 }, type: 0 }],
};
const child = spawn(process.execPath, [plugin, "-port", String(port), "-pluginUUID", UUID, "-registerEvent", "registerPlugin", "-info", JSON.stringify(info)], {
	// Stream Deck starts a plugin in its folder; the SDK reads the manifest there.
	cwd: fileURLToPath(new URL(`../${UUID}.sdPlugin/`, import.meta.url)),
	stdio: ["ignore", "inherit", "inherit"],
});

let failed = 0;
const check = (ok, what) => {
	console.log(`${ok ? "ok  " : "FAIL"} ${what}`);
	if (!ok) failed++;
};
async function until(what, test, ms = 5000) {
	const end = Date.now() + ms;
	while (Date.now() < end) {
		const found = await test();
		if (found) return found;
		await sleep(50);
	}
	check(false, `${what} (timed out)`);
}
const titleOf = (context) => sent.filter((m) => m.event === "setTitle" && m.context === context).at(-1)?.payload.title;
const imageOf = (context) => decodeURIComponent(sent.filter((m) => m.event === "setImage" && m.context === context).at(-1)?.payload.image ?? "");

try {
	if (!(await until("the plugin registers", () => sent.some((m) => m.event === "registerPlugin")))) {
		throw new Error("the plugin didn't connect");
	}
	check(true, "the plugin registered");
	for (const k of keys) {
		reply({ event: "willAppear", action: k.action, context: k.context, device: "deck", payload: payload(k) });
	}
	await until("the card key shows the card", () => titleOf("k-card") === card.name && imageOf("k-card").includes(card.on ? ">ON<" : ">OFF<"));
	check(true, `card key: "${card.name}", ${card.on ? "ON" : "OFF"}`);
	await until("the effect key", () => titleOf("k-effect") === "Emote rain");
	check(true, `effect key: "${titleOf("k-effect")}"`);
	await until("the queue key", () => titleOf("k-queue") === "Queue");
	check(true, `queue key: ${imageOf("k-queue").match(/>([A-Z ]+)</)?.[1]}`);
	await until("a deleted card", () => imageOf("k-gone").includes(">DELETED<") && titleOf("k-gone") === "Old card");
	check(true, "a deleted card's key says DELETED, with its old name");

	// Press the card key: the app switches it, and the key follows.
	reply({ event: "keyDown", action: keys[0].action, context: "k-card", device: "deck", payload: payload(keys[0]) });
	await until("the card switched in the app", async () => (await state()).cards.find((c) => c.id === card.id).on !== card.on);
	check(true, "pressing the card key switched the card");
	await until("the card key redrawn", () => imageOf("k-card").includes(card.on ? ">OFF<" : ">ON<"));
	check(true, "and its key redrew");

	// The effect, switched in the app (as from the panel): the key follows.
	await fetch(`${base}/control/effects/rain/toggle?key=${key}`, { method: "POST" });
	const rainOn = (await state()).effects.find((e) => e.id === "rain").on;
	await until("the effect key follows a change made elsewhere", () => imageOf("k-effect").includes(rainOn ? ">ON<" : ">OFF<"));
	check(true, "the effect key followed a change made elsewhere");

	// The queue key pauses, and resumes.
	reply({ event: "keyDown", action: keys[2].action, context: "k-queue", device: "deck", payload: payload(keys[2]) });
	await until("paused", () => imageOf("k-queue").includes(">PAUSED<"));
	check(true, "the queue key paused the queue");
	reply({ event: "keyDown", action: keys[2].action, context: "k-queue", device: "deck", payload: payload(keys[2]) });
	await until("playing again", () => imageOf("k-queue").includes(">PLAYING<"));
	check(true, "and resumed it");

	// Mute and unmute; a clear limited to follows.
	const muteKey = keys.find((k) => k.context === "k-mute");
	const volume = `>${Math.round(before.sound.volume * 100)}%<`;
	const shows = (muted) => imageOf("k-mute").includes(muted ? ">MUTED<" : volume);
	await until("the mute key", () => shows(before.sound.muted));
	reply({ event: "keyDown", action: muteKey.action, context: "k-mute", device: "deck", payload: payload(muteKey) });
	await until("muted", () => shows(!before.sound.muted));
	check(true, "the mute key switched the sound");
	reply({ event: "keyDown", action: muteKey.action, context: "k-mute", device: "deck", payload: payload(muteKey) });
	await until("back", () => shows(before.sound.muted));
	check(true, `and back (${volume.slice(1, -1)})`);

	await until("the clear-follows key", () => titleOf("k-clear") === "Clear follows" && imageOf("k-clear").includes("WAITING"));
	check(true, `the clear key for follows: ${imageOf("k-clear").match(/>([0-9]+ WAITING)</)?.[1]}`);

	// Not pressed: it would move the app's seen marker for good.
	await until("the history key", () => titleOf("k-history") === "History" && /(NEW|ALL SEEN)</.test(imageOf("k-history")));
	check(true, `the history key: ${imageOf("k-history").match(/>([0-9]+ NEW|ALL SEEN)</)?.[1]}`);

	if (event) {
		reply({ event: "keyDown", action: keys[3].action, context: "k-event", device: "deck", payload: payload(keys[3]) });
		await until("the event fired", () => sent.some((m) => m.event === "showOk" && m.context === "k-event"));
		check(true, `the event key fired "${event}"`);
	} else {
		console.log("skip no card listens for a Custom event, so no event key to press");
	}

	// The settings page: it gets the cards to pick from.
	reply({ event: "propertyInspectorDidAppear", action: keys[0].action, context: "k-card", device: "deck" });
	reply({ event: "sendToPlugin", action: keys[0].action, context: "k-card", payload: { type: "hello" } });
	const view = await until("the settings page's view", () =>
		sent.filter((m) => m.event === "sendToPropertyInspector").map((m) => m.payload).find((p) => p.type === "view" && p.cards.length),
	);
	check(view?.connection === "connected" && view.base === base, `the settings page: ${view?.connection} to ${view?.base}, ${view?.cards.length} cards`);
	check(!JSON.stringify(view).includes(key), "the settings page isn't sent the key");

	// A wrong address: the page is told why, the keys say so.
	reply({ event: "sendToPlugin", action: keys[0].action, context: "k-card", payload: { type: "address", text: `${base}/control/?key=wrong` } });
	await until("the keys say the key is wrong", () => imageOf("k-card").includes(">WRONG KEY<"));
	check(true, "with a wrong key, the keys say WRONG KEY");
	reply({ event: "sendToPlugin", action: keys[0].action, context: "k-card", payload: { type: "address", text: address } });
	await until("connected again", () => imageOf("k-card").includes(">ON<") || imageOf("k-card").includes(">OFF<"));
	check(true, "the right address again: connected");
	check(sent.some((m) => m.event === "setGlobalSettings" && m.payload.address === address), "the address is kept as the plugin's global setting");
} finally {
	// Put things back as they were.
	const now = await state();
	if (now.cards.find((c) => c.id === card.id).on !== card.on) {
		await fetch(`${base}/control/cards/${encodeURIComponent(card.id)}/toggle?key=${key}`, { method: "POST" });
	}
	const rain = before.effects.find((e) => e.id === "rain").on;
	await fetch(`${base}/control/effects/rain/${rain ? "on" : "off"}?key=${key}`, { method: "POST" });
	child.kill();
	server.close();
}
console.log(failed ? `${failed} failed` : "all good");
process.exit(failed ? 1 : 0);
